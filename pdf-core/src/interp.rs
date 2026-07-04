//! Interpréteur de flux de contenu — architecture.md §4.5. Consomme les
//! instructions produites par `content::parse_content_stream` et maintient
//! un état graphique (pile `q`/`Q`, CTM, état texte) pour produire une
//! `DisplayList`.
//!
//! Limitations connues à ce stade — voir sprint.md :
//! - Polices composites (`/Type0`) : gérées (`show_text` branche sur
//!   `Font::is_composite()`, codes 2 octets = CID via `Font::decode_composite`)
//!   mais seulement pour l'`/Encoding` `Identity-H`/`Identity-V` — voir la
//!   doc de module de `font.rs` pour le périmètre exact et ce qui retombe en
//!   best-effort plutôt que d'être rejeté.
//! - Le clip (`W`/`W*`) est suivi dans l'état graphique (intersection des
//!   clips imbriqués, sauvegardé/restauré par `q`/`Q`) et propagé à chaque
//!   `DisplayItem` émis ensuite ; son application effective au rendu se
//!   trouve dans `pdf-render`.
//! - Les patterns, shadings et le contenu marqué sont ignorés.
//! - Les images XObject ne sont pas décodées (position seulement).

use std::rc::Rc;

use crate::content::{parse_content_stream, ContentInstruction};
use crate::display::{
    ClipPath, ClipStack, Color, DisplayItem, DisplayList, FillRule, Matrix, PaintOp, PathSegment,
};
use crate::document::Document;
use crate::error::Result;
use crate::font::Font;
use crate::object::{Dictionary, Object};

const MAX_XOBJECT_DEPTH: usize = 12;

#[derive(Debug, Clone)]
struct GraphicsState {
    ctm: Matrix,
    fill_color: Color,
    stroke_color: Color,
    line_width: f64,
    char_spacing: f64,
    word_spacing: f64,
    h_scale: f64,
    leading: f64,
    font: Option<String>,
    font_size: f64,
    text_rise: f64,
    /// Chaîne de clips actifs (intersection) — `None` = pas de clip. Fait
    /// partie de l'état graphique : sauvegardé/restauré par `q`/`Q` comme le
    /// reste de `GraphicsState`.
    clip: Option<ClipStack>,
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self {
            ctm: Matrix::IDENTITY,
            fill_color: Color::default(),
            stroke_color: Color::default(),
            line_width: 1.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            h_scale: 1.0,
            leading: 0.0,
            font: None,
            font_size: 0.0,
            text_rise: 0.0,
            clip: None,
        }
    }
}

/// Avance de repli quand aucune police n'est résolvable dans les ressources
/// (référence manquante, police composite `/Type0` non supportée...) — dans
/// le cas courant d'une police simple résolue, `font::Font::decode_simple`
/// fournit une largeur réelle (`/Widths` ou table Helvetica AFM).
const PLACEHOLDER_GLYPH_WIDTH_PER_MILLE: f64 = 500.0;

pub struct Interpreter<'a> {
    doc: &'a Document,
    display: DisplayList,
    gs: GraphicsState,
    gs_stack: Vec<GraphicsState>,
    current_path: Vec<PathSegment>,
    current_point: (f64, f64),
    subpath_start: (f64, f64),
    pending_clip: Option<FillRule>,
    text_matrix: Matrix,
    text_line_matrix: Matrix,
    resources_stack: Vec<Dictionary>,
    depth: usize,
}

impl<'a> Interpreter<'a> {
    pub fn new(doc: &'a Document, resources: Dictionary) -> Self {
        Self {
            doc,
            display: DisplayList::new(),
            gs: GraphicsState::default(),
            gs_stack: Vec::new(),
            current_path: Vec::new(),
            current_point: (0.0, 0.0),
            subpath_start: (0.0, 0.0),
            pending_clip: None,
            text_matrix: Matrix::IDENTITY,
            text_line_matrix: Matrix::IDENTITY,
            resources_stack: vec![resources],
            depth: 0,
        }
    }

    /// Interprète le contenu d'une page entière et retourne la display list.
    pub fn run_page(
        doc: &'a Document,
        resources: Dictionary,
        content: &[u8],
    ) -> Result<DisplayList> {
        let mut interp = Self::new(doc, resources);
        interp.run(content)?;
        Ok(interp.display)
    }

    fn run(&mut self, content: &[u8]) -> Result<()> {
        let instructions = parse_content_stream(content)?;
        for instr in instructions {
            self.exec(&instr)?;
        }
        Ok(())
    }

    fn exec(&mut self, instr: &ContentInstruction) -> Result<()> {
        let ops = &instr.operands;
        match instr.operator.as_str() {
            "q" => self.gs_stack.push(self.gs.clone()),
            "Q" => {
                if let Some(gs) = self.gs_stack.pop() {
                    self.gs = gs;
                }
            }
            "cm" => {
                if let Some(m) = matrix_from(ops) {
                    self.gs.ctm = m.then(&self.gs.ctm);
                }
            }
            "w" => self.gs.line_width = num(ops, 0),

            // Construction de chemin.
            "m" => {
                let p = (num(ops, 0), num(ops, 1));
                self.current_point = p;
                self.subpath_start = p;
                self.current_path.push(PathSegment::MoveTo(self.tp(p)));
            }
            "l" => {
                let p = (num(ops, 0), num(ops, 1));
                self.current_point = p;
                self.current_path.push(PathSegment::LineTo(self.tp(p)));
            }
            "c" => {
                let c1 = (num(ops, 0), num(ops, 1));
                let c2 = (num(ops, 2), num(ops, 3));
                let to = (num(ops, 4), num(ops, 5));
                self.current_path.push(PathSegment::CurveTo {
                    c1: self.tp(c1),
                    c2: self.tp(c2),
                    to: self.tp(to),
                });
                self.current_point = to;
            }
            "v" => {
                let c1 = self.current_point;
                let c2 = (num(ops, 0), num(ops, 1));
                let to = (num(ops, 2), num(ops, 3));
                self.current_path.push(PathSegment::CurveTo {
                    c1: self.tp(c1),
                    c2: self.tp(c2),
                    to: self.tp(to),
                });
                self.current_point = to;
            }
            "y" => {
                let c1 = (num(ops, 0), num(ops, 1));
                let to = (num(ops, 2), num(ops, 3));
                self.current_path.push(PathSegment::CurveTo {
                    c1: self.tp(c1),
                    c2: self.tp(to),
                    to: self.tp(to),
                });
                self.current_point = to;
            }
            "h" => {
                self.current_path.push(PathSegment::ClosePath);
                self.current_point = self.subpath_start;
            }
            "re" => {
                let (x, y, w, h) = (num(ops, 0), num(ops, 1), num(ops, 2), num(ops, 3));
                self.current_path.push(PathSegment::MoveTo(self.tp((x, y))));
                self.current_path
                    .push(PathSegment::LineTo(self.tp((x + w, y))));
                self.current_path
                    .push(PathSegment::LineTo(self.tp((x + w, y + h))));
                self.current_path
                    .push(PathSegment::LineTo(self.tp((x, y + h))));
                self.current_path.push(PathSegment::ClosePath);
                self.current_point = (x, y);
                self.subpath_start = (x, y);
            }

            // Clip : la sémantique PDF (ISO 32000-1 §8.5.4) veut que le
            // nouveau clip prenne effet seulement après le prochain
            // opérateur de peinture (souvent `n`, qui ne peint rien).
            "W" => self.pending_clip = Some(FillRule::NonZero),
            "W*" => self.pending_clip = Some(FillRule::EvenOdd),

            // Peinture de chemin.
            "S" => self.paint_path(PaintOp::Stroke, FillRule::NonZero),
            "s" => {
                self.current_path.push(PathSegment::ClosePath);
                self.paint_path(PaintOp::Stroke, FillRule::NonZero);
            }
            "f" | "F" => self.paint_path(PaintOp::Fill, FillRule::NonZero),
            "f*" => self.paint_path(PaintOp::Fill, FillRule::EvenOdd),
            "B" => self.paint_path(PaintOp::FillStroke, FillRule::NonZero),
            "B*" => self.paint_path(PaintOp::FillStroke, FillRule::EvenOdd),
            "b" => {
                self.current_path.push(PathSegment::ClosePath);
                self.paint_path(PaintOp::FillStroke, FillRule::NonZero);
            }
            "b*" => {
                self.current_path.push(PathSegment::ClosePath);
                self.paint_path(PaintOp::FillStroke, FillRule::EvenOdd);
            }
            "n" => self.paint_path(PaintOp::None, FillRule::NonZero),

            // Couleur.
            "g" => self.gs.fill_color = Color::Gray(num(ops, 0)),
            "G" => self.gs.stroke_color = Color::Gray(num(ops, 0)),
            "rg" => self.gs.fill_color = Color::Rgb(num(ops, 0), num(ops, 1), num(ops, 2)),
            "RG" => self.gs.stroke_color = Color::Rgb(num(ops, 0), num(ops, 1), num(ops, 2)),
            "k" => {
                self.gs.fill_color = Color::Cmyk(num(ops, 0), num(ops, 1), num(ops, 2), num(ops, 3))
            }
            "K" => {
                self.gs.stroke_color =
                    Color::Cmyk(num(ops, 0), num(ops, 1), num(ops, 2), num(ops, 3))
            }
            "sc" | "scn" => {
                if let Some(c) = color_from_components(ops) {
                    self.gs.fill_color = c;
                }
            }
            "SC" | "SCN" => {
                if let Some(c) = color_from_components(ops) {
                    self.gs.stroke_color = c;
                }
            }

            // État graphique étendu (partiel : /LW seulement pour l'instant).
            "gs" => self.apply_ext_gstate(ops)?,

            // Texte.
            "BT" => {
                self.text_matrix = Matrix::IDENTITY;
                self.text_line_matrix = Matrix::IDENTITY;
            }
            "ET" => {}
            "Tf" => {
                if let Some(Object::Name(name)) = ops.first() {
                    self.gs.font = Some(name.clone());
                }
                self.gs.font_size = num(ops, 1);
            }
            "Tc" => self.gs.char_spacing = num(ops, 0),
            "Tw" => self.gs.word_spacing = num(ops, 0),
            "Tz" => self.gs.h_scale = num(ops, 0) / 100.0,
            "TL" => self.gs.leading = num(ops, 0),
            "Ts" => self.gs.text_rise = num(ops, 0),
            "Tr" => {} // mode de rendu du texte : ignoré (pas de rendu réel pour l'instant).
            "Td" => {
                let m = Matrix::translation(num(ops, 0), num(ops, 1)).then(&self.text_line_matrix);
                self.text_line_matrix = m;
                self.text_matrix = m;
            }
            "TD" => {
                self.gs.leading = -num(ops, 1);
                let m = Matrix::translation(num(ops, 0), num(ops, 1)).then(&self.text_line_matrix);
                self.text_line_matrix = m;
                self.text_matrix = m;
            }
            "Tm" => {
                let m = matrix_from(ops).unwrap_or(Matrix::IDENTITY);
                self.text_line_matrix = m;
                self.text_matrix = m;
            }
            "T*" => {
                let m = Matrix::translation(0.0, -self.gs.leading).then(&self.text_line_matrix);
                self.text_line_matrix = m;
                self.text_matrix = m;
            }
            "Tj" => {
                if let Some(Object::String(s)) = ops.first() {
                    self.show_text(s);
                }
            }
            "'" => {
                let m = Matrix::translation(0.0, -self.gs.leading).then(&self.text_line_matrix);
                self.text_line_matrix = m;
                self.text_matrix = m;
                if let Some(Object::String(s)) = ops.first() {
                    self.show_text(s);
                }
            }
            "\"" => {
                self.gs.word_spacing = num(ops, 0);
                self.gs.char_spacing = num(ops, 1);
                let m = Matrix::translation(0.0, -self.gs.leading).then(&self.text_line_matrix);
                self.text_line_matrix = m;
                self.text_matrix = m;
                if let Some(Object::String(s)) = ops.get(2) {
                    self.show_text(s);
                }
            }
            "TJ" => {
                if let Some(Object::Array(items)) = ops.first() {
                    for item in items {
                        match item {
                            Object::String(s) => self.show_text(s),
                            other => {
                                if let Some(adj) =
                                    other.as_int().map(|n| n as f64).or(match other {
                                        Object::Real(f) => Some(*f),
                                        _ => None,
                                    })
                                {
                                    let dx = -adj / 1000.0 * self.gs.font_size * self.gs.h_scale;
                                    self.text_matrix =
                                        Matrix::translation(dx, 0.0).then(&self.text_matrix);
                                }
                            }
                        }
                    }
                }
            }

            "Do" => self.do_xobject(ops)?,

            // Contenu marqué, groupes de compatibilité, autres paramètres
            // d'état graphique (J, j, M, d, ri, i) : ignorés à ce stade.
            _ => {}
        }
        Ok(())
    }

    fn tp(&self, (x, y): (f64, f64)) -> (f64, f64) {
        self.gs.ctm.apply(x, y)
    }

    fn paint_path(&mut self, paint: PaintOp, fill_rule: FillRule) {
        let clip_rule = self.pending_clip;
        let sets_clip = clip_rule.is_some();
        if (paint != PaintOp::None || sets_clip) && !self.current_path.is_empty() {
            let segments = std::mem::take(&mut self.current_path);
            // Le nouveau clip (s'il y en a un) doit intersecter les clips déjà
            // actifs, mais ne s'applique qu'aux items *suivants* — celui-ci
            // est donc peint avec le clip *avant* mise à jour.
            let clip_before = self.gs.clip.clone();
            if let Some(rule) = clip_rule {
                let mut stack: Vec<ClipPath> = self.gs.clip.as_deref().cloned().unwrap_or_default();
                stack.push(ClipPath {
                    segments: segments.clone(),
                    fill_rule: rule,
                });
                self.gs.clip = Some(Rc::new(stack));
            }
            self.display.items.push(DisplayItem::Path {
                segments,
                paint,
                fill_rule,
                fill_color: self.gs.fill_color,
                stroke_color: self.gs.stroke_color,
                line_width: self.gs.line_width,
                sets_clip,
                clip: clip_before,
            });
        }
        self.current_path.clear();
        self.pending_clip = None;
    }

    fn show_text(&mut self, bytes: &[u8]) {
        let font_name = self.gs.font.clone().unwrap_or_default();
        // Rechargé à chaque appel plutôt que mis en cache : simple et correct
        // (pas de risque d'incohérence entre resources_stack imbriquées),
        // au prix d'un re-parsing du dict de police par Tj/TJ — acceptable
        // tant que les documents restent de taille modeste (voir sprint.md).
        let font = self
            .lookup_resource("Font", &font_name)
            .ok()
            .flatten()
            .and_then(|obj| obj.as_dict().cloned())
            .and_then(|dict| Font::load(self.doc, &dict).ok());

        match &font {
            // Police composite (`/Type0`) : codes 2 octets = CID (voir
            // `Font::decode_composite` pour le périmètre exact,
            // Identity-H/V). L'espacement de mot (`Tw`) ne s'applique qu'au
            // code 1 octet 32 (ISO 32000-1 §9.3.3) : jamais le cas ici.
            Some(f) if f.is_composite() => {
                for cid in f.decode_composite(bytes) {
                    let (unicode, width_per_mille) = f.cid_metrics(cid);
                    let outline = f.cid_glyph_outline(cid);
                    self.emit_glyph(
                        &font_name,
                        cid,
                        unicode,
                        width_per_mille,
                        false,
                        outline,
                        false,
                    );
                }
            }
            Some(f) => {
                for &code in bytes {
                    let (unicode, width_per_mille) = f.decode_simple(code);
                    let outline = f.glyph_outline(code, unicode);
                    self.emit_glyph(
                        &font_name,
                        code as u32,
                        unicode,
                        width_per_mille,
                        false,
                        outline,
                        code == b' ',
                    );
                }
            }
            None => {
                for &code in bytes {
                    self.emit_glyph(
                        &font_name,
                        code as u32,
                        None,
                        PLACEHOLDER_GLYPH_WIDTH_PER_MILLE,
                        true,
                        None,
                        code == b' ',
                    );
                }
            }
        }
    }

    /// Émet un `DisplayItem::Glyph` pour un code/CID déjà résolu en
    /// `(unicode, largeur)` et avance `text_matrix` en conséquence — partagé
    /// entre les trois chemins de `show_text` (police simple, composite,
    /// non résolue) pour ne pas dupliquer le calcul de transform/avance.
    #[allow(clippy::too_many_arguments)]
    fn emit_glyph(
        &mut self,
        font_name: &str,
        code: u32,
        unicode: Option<char>,
        width_per_mille: f64,
        advance_is_estimated: bool,
        outline: Option<Vec<PathSegment>>,
        apply_word_spacing: bool,
    ) {
        let scale = Matrix::new(
            self.gs.font_size * self.gs.h_scale,
            0.0,
            0.0,
            self.gs.font_size,
            0.0,
            self.gs.text_rise,
        );
        let transform = scale.then(&self.text_matrix).then(&self.gs.ctm);

        self.display.items.push(DisplayItem::Glyph {
            font: font_name.to_string(),
            code,
            unicode,
            transform,
            color: self.gs.fill_color,
            advance_is_estimated,
            outline,
            clip: self.gs.clip.clone(),
        });

        let word_spacing = if apply_word_spacing {
            self.gs.word_spacing
        } else {
            0.0
        };
        let advance =
            (width_per_mille / 1000.0 * self.gs.font_size + self.gs.char_spacing + word_spacing)
                * self.gs.h_scale;
        self.text_matrix = Matrix::translation(advance, 0.0).then(&self.text_matrix);
    }

    fn apply_ext_gstate(&mut self, ops: &[Object]) -> Result<()> {
        let Some(Object::Name(name)) = ops.first() else {
            return Ok(());
        };
        let Some(ext_gstate) = self.lookup_resource("ExtGState", name)? else {
            return Ok(());
        };
        if let Some(dict) = ext_gstate.as_dict() {
            if let Some(lw) = dict.get("LW").and_then(|o| o.as_int()) {
                self.gs.line_width = lw as f64;
            }
        }
        Ok(())
    }

    fn do_xobject(&mut self, ops: &[Object]) -> Result<()> {
        let Some(Object::Name(name)) = ops.first() else {
            return Ok(());
        };
        let Some(xobj) = self.lookup_resource("XObject", name)? else {
            return Ok(());
        };
        let Object::Stream(stream) = &xobj else {
            return Ok(());
        };

        match stream.dict.get("Subtype").and_then(|o| o.as_name()) {
            Some("Image") => {
                // Une image non supportée (CCITT, JBIG2, espace colorimétrique
                // indexé...) ne doit pas faire échouer tout le reste de la
                // page : on dégrade en `pixels: None` (voir image.rs).
                let pixels = crate::image::decode_image(self.doc, stream).ok();
                self.display.items.push(DisplayItem::Image {
                    resource: name.clone(),
                    transform: self.gs.ctm,
                    pixels,
                    clip: self.gs.clip.clone(),
                });
            }
            Some("Form") => {
                if self.depth >= MAX_XOBJECT_DEPTH {
                    return Ok(()); // garde-fou contre les formes auto-référentes.
                }
                let decoded = crate::filters::decode_stream(stream)?;
                let form_resources = match stream.dict.get("Resources") {
                    Some(obj) => self
                        .doc
                        .get(obj)?
                        .as_dict()
                        .cloned()
                        .unwrap_or_else(Dictionary::new),
                    None => self.resources_stack.last().cloned().unwrap_or_default(),
                };
                let form_matrix = stream
                    .dict
                    .get("Matrix")
                    .and_then(|o| o.as_array())
                    .and_then(matrix_from)
                    .unwrap_or(Matrix::IDENTITY);

                let saved_gs = self.gs.clone();
                self.gs.ctm = form_matrix.then(&self.gs.ctm);
                self.resources_stack.push(form_resources);
                self.depth += 1;

                self.run(&decoded)?;

                self.depth -= 1;
                self.resources_stack.pop();
                self.gs = saved_gs;
            }
            _ => {}
        }
        Ok(())
    }

    fn lookup_resource(&self, category: &str, name: &str) -> Result<Option<Object>> {
        let Some(resources) = self.resources_stack.last() else {
            return Ok(None);
        };
        let Some(cat_obj) = resources.get(category) else {
            return Ok(None);
        };
        let cat_dict = self.doc.get(cat_obj)?;
        let Some(cat_dict) = cat_dict.as_dict() else {
            return Ok(None);
        };
        let Some(entry) = cat_dict.get(name) else {
            return Ok(None);
        };
        Ok(Some(self.doc.get(entry)?))
    }
}

fn num(ops: &[Object], index: usize) -> f64 {
    ops.get(index)
        .map(|o| match o {
            Object::Integer(n) => *n as f64,
            Object::Real(f) => *f,
            _ => 0.0,
        })
        .unwrap_or(0.0)
}

fn matrix_from(ops: &[Object]) -> Option<Matrix> {
    if ops.len() < 6 {
        return None;
    }
    Some(Matrix::new(
        num(ops, 0),
        num(ops, 1),
        num(ops, 2),
        num(ops, 3),
        num(ops, 4),
        num(ops, 5),
    ))
}

/// Interprète les opérandes de `sc`/`scn`/`SC`/`SCN` : les composantes
/// numériques déterminent l'espace colorimétrique implicite par leur nombre
/// (1 = Gray, 3 = Rgb, 4 = Cmyk). Un éventuel nom de motif (`Pattern`) en
/// dernier opérande est ignoré (patterns non supportés — voir sprint.md).
fn color_from_components(ops: &[Object]) -> Option<Color> {
    let numbers: Vec<f64> = ops
        .iter()
        .filter_map(|o| match o {
            Object::Integer(n) => Some(*n as f64),
            Object::Real(f) => Some(*f),
            _ => None,
        })
        .collect();
    match numbers.len() {
        1 => Some(Color::Gray(numbers[0])),
        3 => Some(Color::Rgb(numbers[0], numbers[1], numbers[2])),
        4 => Some(Color::Cmyk(numbers[0], numbers[1], numbers[2], numbers[3])),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    fn doc_with_page(content: &str) -> (Document, Dictionary) {
        let content_bytes = content.as_bytes();
        let body = format!(
            "%PDF-1.7\n\
             1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
             2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n\
             3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R >>\nendobj\n\
             4 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n",
            content_bytes.len(),
            content
        );
        let mut bytes = body.into_bytes();
        let offset_of = |data: &[u8], needle: &str| -> usize {
            data.windows(needle.len())
                .position(|w| w == needle.as_bytes())
                .unwrap()
        };
        let offsets: Vec<usize> = (1..=4)
            .map(|n| offset_of(&bytes, &format!("{n} 0 obj")))
            .collect();
        let xref_offset = bytes.len();
        let mut xref = format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1);
        for off in &offsets {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        xref.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
            offsets.len() + 1,
            xref_offset
        ));
        bytes.extend_from_slice(xref.as_bytes());

        let doc = Document::open(bytes).unwrap();
        let page = doc.page(0).unwrap();
        (doc, page.resources)
    }

    #[test]
    fn rectangle_fill_produces_path_item() {
        let (doc, resources) = doc_with_page("1 0 0 rg 100 200 50 60 re f");
        let display =
            Interpreter::run_page(&doc, resources, b"1 0 0 rg 100 200 50 60 re f").unwrap();
        assert_eq!(display.items.len(), 1);
        match &display.items[0] {
            DisplayItem::Path {
                segments,
                paint,
                fill_color,
                ..
            } => {
                assert_eq!(*paint, PaintOp::Fill);
                assert_eq!(*fill_color, Color::Rgb(1.0, 0.0, 0.0));
                // re + implicit close = 5 segments (MoveTo,Line,Line,Line,Close).
                assert_eq!(segments.len(), 5);
                assert_eq!(segments[0], PathSegment::MoveTo((100.0, 200.0)));
            }
            other => panic!("expected Path, got {other:?}"),
        }
        let _ = doc; // conserve la Document en vie pour la durée du test.
    }

    #[test]
    fn text_showing_emits_one_glyph_per_byte() {
        let (doc, resources) = doc_with_page("BT /F1 24 Tf 72 720 Td (Hi) Tj ET");
        let display =
            Interpreter::run_page(&doc, resources, b"BT /F1 24 Tf 72 720 Td (Hi) Tj ET").unwrap();
        let glyphs: Vec<&DisplayItem> = display
            .items
            .iter()
            .filter(|i| matches!(i, DisplayItem::Glyph { .. }))
            .collect();
        assert_eq!(glyphs.len(), 2);
        if let DisplayItem::Glyph { font, code, .. } = glyphs[0] {
            assert_eq!(font, "F1");
            assert_eq!(*code, b'H' as u32);
        } else {
            unreachable!();
        }
    }

    /// Symétrique de `text_showing_emits_one_glyph_per_byte`, pour une
    /// police composite (`/Type0`/`Identity-H`) : la chaîne hexadécimale
    /// `<00090041>` doit produire deux glyphes (CID 9 et CID 0x41), pas
    /// quatre (un par octet) — preuve que `show_text` détecte réellement
    /// `Font::is_composite()` et découpe en CIDs 2 octets plutôt que de
    /// retomber sur l'itération par octet du chemin police simple/placeholder.
    #[test]
    fn text_showing_on_a_composite_font_emits_one_glyph_per_two_byte_cid() {
        let content = "BT /F1 24 Tf 72 720 Td <00090041> Tj ET";
        let body = format!(
            "%PDF-1.7\n\
             1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
             2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n\
             3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>\nendobj\n\
             4 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n\
             5 0 obj\n<< /Type /Font /Subtype /Type0 /Encoding /Identity-H \
             /DescendantFonts [6 0 R] >>\nendobj\n\
             6 0 obj\n<< /Type /Font /Subtype /CIDFontType2 /DW 1000 /W [9 [600]] >>\nendobj\n",
            content.len(),
            content
        );
        let mut bytes = body.into_bytes();
        let offset_of = |data: &[u8], needle: &str| -> usize {
            data.windows(needle.len())
                .position(|w| w == needle.as_bytes())
                .unwrap()
        };
        let offsets: Vec<usize> = (1..=6)
            .map(|n| offset_of(&bytes, &format!("{n} 0 obj")))
            .collect();
        let xref_offset = bytes.len();
        let mut xref = format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len() + 1);
        for off in &offsets {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        xref.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
            offsets.len() + 1,
            xref_offset
        ));
        bytes.extend_from_slice(xref.as_bytes());

        let doc = Document::open(bytes).unwrap();
        let page = doc.page(0).unwrap();
        let display = Interpreter::run_page(&doc, page.resources, content.as_bytes()).unwrap();

        let glyphs: Vec<&DisplayItem> = display
            .items
            .iter()
            .filter(|i| matches!(i, DisplayItem::Glyph { .. }))
            .collect();
        assert_eq!(
            glyphs.len(),
            2,
            "expected 2 glyphs (one per CID), not one per byte"
        );

        let DisplayItem::Glyph {
            code: first_code, ..
        } = glyphs[0]
        else {
            unreachable!()
        };
        let DisplayItem::Glyph {
            code: second_code, ..
        } = glyphs[1]
        else {
            unreachable!()
        };
        assert_eq!(*first_code, 0x0009);
        assert_eq!(*second_code, 0x0041);
    }

    #[test]
    fn cm_concatenation_transforms_subsequent_points() {
        let (doc, resources) = doc_with_page("2 0 0 2 10 10 cm 0 0 5 5 re f");
        let display =
            Interpreter::run_page(&doc, resources, b"2 0 0 2 10 10 cm 0 0 5 5 re f").unwrap();
        match &display.items[0] {
            DisplayItem::Path { segments, .. } => {
                // (0,0) -> scale 2 + translate (10,10) => (10,10).
                assert_eq!(segments[0], PathSegment::MoveTo((10.0, 10.0)));
                // (5,0) -> (2*5+10, 2*0+10) = (20,10).
                assert_eq!(segments[1], PathSegment::LineTo((20.0, 10.0)));
            }
            other => panic!("expected Path, got {other:?}"),
        }
    }

    #[test]
    fn q_q_restores_graphics_state() {
        let (doc, resources) = doc_with_page("1 0 0 rg q 0 1 0 rg Q 10 10 5 5 re f");
        let display =
            Interpreter::run_page(&doc, resources, b"1 0 0 rg q 0 1 0 rg Q 10 10 5 5 re f")
                .unwrap();
        match &display.items[0] {
            DisplayItem::Path { fill_color, .. } => {
                assert_eq!(*fill_color, Color::Rgb(1.0, 0.0, 0.0));
            }
            other => panic!("expected Path, got {other:?}"),
        }
    }

    #[test]
    fn clip_operator_attaches_clip_only_to_subsequent_items() {
        // `W n` fixe un clip 10..50 sans rien peindre ; le rectangle peint
        // ensuite doit porter ce clip, pas l'inverse.
        let content = "10 10 40 40 re W n 0 0 0 rg 0 0 100 100 re f";
        let (doc, resources) = doc_with_page(content);
        let display = Interpreter::run_page(&doc, resources, content.as_bytes()).unwrap();

        // `W n` seul ne produit pas de path peint mais un `sets_clip`+paint=None.
        let clip_setter = display
            .items
            .iter()
            .find(|i| {
                matches!(
                    i,
                    DisplayItem::Path {
                        sets_clip: true,
                        ..
                    }
                )
            })
            .expect("expected a clip-setting path");
        if let DisplayItem::Path { clip, .. } = clip_setter {
            assert!(
                clip.is_none(),
                "the clip-setting item itself has no clip yet"
            );
        }

        let filled = display
            .items
            .iter()
            .find(|i| {
                matches!(
                    i,
                    DisplayItem::Path {
                        paint: PaintOp::Fill,
                        ..
                    }
                )
            })
            .expect("expected the filled rect");
        if let DisplayItem::Path { clip, .. } = filled {
            let clip = clip.as_ref().expect("filled rect should carry the clip");
            assert_eq!(clip.len(), 1);
            assert_eq!(clip[0].fill_rule, FillRule::NonZero);
        } else {
            unreachable!();
        }
    }

    #[test]
    fn nested_clips_intersect_and_restore_on_q() {
        // Clip 0..50 imbriqué avec un clip 20..80 : le rectangle peint à
        // l'intérieur des deux doit porter une chaîne de 2 clips ; après `Q`,
        // le rectangle suivant ne doit plus porter que le premier clip.
        let content = "0 0 50 50 re W n q 20 20 60 60 re W n 0 0 0 rg 0 0 100 100 re f Q 0 0 0 rg 0 0 1 1 re f";
        let (doc, resources) = doc_with_page(content);
        let display = Interpreter::run_page(&doc, resources, content.as_bytes()).unwrap();

        let fills: Vec<&DisplayItem> = display
            .items
            .iter()
            .filter(|i| {
                matches!(
                    i,
                    DisplayItem::Path {
                        paint: PaintOp::Fill,
                        ..
                    }
                )
            })
            .collect();
        assert_eq!(fills.len(), 2);

        if let DisplayItem::Path { clip, .. } = fills[0] {
            assert_eq!(clip.as_ref().unwrap().len(), 2);
        } else {
            unreachable!();
        }
        if let DisplayItem::Path { clip, .. } = fills[1] {
            assert_eq!(clip.as_ref().unwrap().len(), 1);
        } else {
            unreachable!();
        }
    }

    #[test]
    fn real_fixture_recovers_unicode_text_and_real_widths() {
        let bytes = include_bytes!("../tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let doc = Document::open(bytes).unwrap();
        let page = doc.page(0).unwrap();
        let content = doc.page_content(&page).unwrap();
        let display = Interpreter::run_page(&doc, page.resources.clone(), &content).unwrap();

        let text: String = display
            .items
            .iter()
            .filter_map(|item| match item {
                DisplayItem::Glyph { unicode, .. } => *unicode,
                _ => None,
            })
            .collect();
        assert_eq!(text, "Page 1 - Hello, PDF Manager!");

        assert!(display.items.iter().all(|item| !matches!(
            item,
            DisplayItem::Glyph {
                advance_is_estimated: true,
                ..
            }
        )));
    }

    /// Bout en bout sur un vrai `/Type0`/`CIDFontType2` (`/Identity-H`,
    /// `/CIDToGIDMap /Identity`, sous-ensemble TrueType Monaco réel généré
    /// avec `fonttools subset` — voir
    /// `pdf-core/tests/fixtures/README.md`) : "AB" doit se recomposer
    /// exactement via `/ToUnicode`, avec un vrai contour résolu pour chaque
    /// glyphe (pas `None`) — preuve que le chemin composite de `show_text`
    /// fonctionne sur un PDF réel généré par un outil tiers, pas seulement
    /// sur les fixtures synthétiques de `font.rs`.
    #[test]
    fn real_type0_fixture_recovers_unicode_text_and_real_outlines() {
        let bytes = include_bytes!("../tests/fixtures/type0_cid_truetype.pdf").to_vec();
        let doc = Document::open(bytes).unwrap();
        let page = doc.page(0).unwrap();
        let content = doc.page_content(&page).unwrap();
        let display = Interpreter::run_page(&doc, page.resources.clone(), &content).unwrap();

        let glyphs: Vec<&DisplayItem> = display
            .items
            .iter()
            .filter(|i| matches!(i, DisplayItem::Glyph { .. }))
            .collect();
        assert_eq!(glyphs.len(), 2);

        let text: String = glyphs
            .iter()
            .filter_map(|item| match item {
                DisplayItem::Glyph { unicode, .. } => *unicode,
                _ => None,
            })
            .collect();
        assert_eq!(text, "AB");

        for glyph in &glyphs {
            let DisplayItem::Glyph {
                outline,
                advance_is_estimated,
                ..
            } = glyph
            else {
                unreachable!()
            };
            assert!(
                outline.as_ref().is_some_and(|o| !o.is_empty()),
                "expected a real outline for each glyph of the Type0 fixture"
            );
            assert!(!advance_is_estimated);
        }
    }

    /// Symétrique de `real_type0_fixture_recovers_unicode_text_and_real_outlines`
    /// pour l'autre variante `/Type0` citée comme manquante dans sprint.md
    /// (Sprint 7-8) : `/CIDFontType0` (CFF CID-keyed, `/FontFile3` sous-type
    /// `CIDFontType0C`), où le code du flux de contenu est le CID résolu via
    /// le charset interne de la table CFF plutôt que via `/CIDToGIDMap`
    /// (`font.rs::cid_glyph_outline`). Sous-ensemble réel de Hiragino Sans GB
    /// (police système CJK CID-keyed, `/ROS Adobe-GB1`).
    #[test]
    fn real_cid_keyed_cff_fixture_recovers_unicode_text_and_real_outlines() {
        let bytes = include_bytes!("../tests/fixtures/type0_cid_cff.pdf").to_vec();
        let doc = Document::open(bytes).unwrap();
        let page = doc.page(0).unwrap();
        let content = doc.page_content(&page).unwrap();
        let display = Interpreter::run_page(&doc, page.resources.clone(), &content).unwrap();

        let glyphs: Vec<&DisplayItem> = display
            .items
            .iter()
            .filter(|i| matches!(i, DisplayItem::Glyph { .. }))
            .collect();
        assert_eq!(glyphs.len(), 2);

        let text: String = glyphs
            .iter()
            .filter_map(|item| match item {
                DisplayItem::Glyph { unicode, .. } => *unicode,
                _ => None,
            })
            .collect();
        assert_eq!(text, "你好");

        for glyph in &glyphs {
            let DisplayItem::Glyph { outline, .. } = glyph else {
                unreachable!()
            };
            assert!(
                outline.as_ref().is_some_and(|o| !o.is_empty()),
                "expected a real outline for each glyph of the CIDFontType0 fixture"
            );
        }
    }

    #[test]
    fn real_fixture_decodes_embedded_jpeg_image() {
        let bytes = include_bytes!("../tests/fixtures/image_jpeg.pdf").to_vec();
        let doc = Document::open(bytes).unwrap();
        let page = doc.page(0).unwrap();
        let content = doc.page_content(&page).unwrap();
        let display = Interpreter::run_page(&doc, page.resources.clone(), &content).unwrap();

        let image = display
            .items
            .iter()
            .find_map(|item| match item {
                DisplayItem::Image { pixels, .. } => pixels.as_ref(),
                _ => None,
            })
            .expect("expected a decoded image in the DisplayList");
        assert_eq!(image.width, 120);
        assert_eq!(image.height, 80);
        assert_eq!(image.rgba.len(), 120 * 80 * 4);
    }

    #[test]
    fn real_fixture_decodes_smask_alpha_channel() {
        let bytes = include_bytes!("../tests/fixtures/image_smask.pdf").to_vec();
        let doc = Document::open(bytes).unwrap();
        let page = doc.page(0).unwrap();
        let content = doc.page_content(&page).unwrap();
        let display = Interpreter::run_page(&doc, page.resources.clone(), &content).unwrap();

        let image = display
            .items
            .iter()
            .find_map(|item| match item {
                DisplayItem::Image { pixels, .. } => pixels.as_ref(),
                _ => None,
            })
            .expect("expected a decoded image in the DisplayList");

        // L'image source est uniformément rouge cramoisi à alpha ~128/255
        // (voir pdf-core/tests/fixtures/README.md) : le canal alpha ne doit
        // donc pas être resté à 255 partout (ce qui indiquerait que le
        // /SMask a été ignoré).
        let alphas: Vec<u8> = image.rgba.chunks_exact(4).map(|p| p[3]).collect();
        assert!(alphas.iter().any(|&a| a < 250));
    }
}
