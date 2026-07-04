//! Interpréteur de flux de contenu — architecture.md §4.5. Consomme les
//! instructions produites par `content::parse_content_stream` et maintient
//! un état graphique (pile `q`/`Q`, CTM, état texte) pour produire une
//! `DisplayList`.
//!
//! Limitations connues à ce stade — voir sprint.md :
//! - Polices composites (`/Type0`, CID, codes 2 octets) : non gérées par
//!   `font::Font` ; repli sur l'avance placeholder et `unicode: None`
//!   (`DisplayItem::Glyph::advance_is_estimated` le signale).
//! - `/ToUnicode` n'est pas lu (seul `/Encoding`+`/Differences` l'est).
//! - Le clip (`W`/`W*`) est signalé (`sets_clip`) mais pas appliqué.
//! - Les patterns, shadings et le contenu marqué sont ignorés.
//! - Les images XObject ne sont pas décodées (position seulement).

use crate::content::{parse_content_stream, ContentInstruction};
use crate::display::{Color, DisplayItem, DisplayList, FillRule, Matrix, PaintOp, PathSegment};
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
    pending_clip: bool,
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
            pending_clip: false,
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

            // Clip.
            "W" | "W*" => self.pending_clip = true,

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
        let sets_clip = self.pending_clip;
        if (paint != PaintOp::None || sets_clip) && !self.current_path.is_empty() {
            self.display.items.push(DisplayItem::Path {
                segments: std::mem::take(&mut self.current_path),
                paint,
                fill_rule,
                fill_color: self.gs.fill_color,
                stroke_color: self.gs.stroke_color,
                line_width: self.gs.line_width,
                sets_clip,
            });
        }
        self.current_path.clear();
        self.pending_clip = false;
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
            .and_then(|dict| Font::load(self.doc, &dict).ok())
            .filter(|f| !f.is_composite()); // polices composites (Type0) : repli placeholder.

        for &code in bytes {
            let scale = Matrix::new(
                self.gs.font_size * self.gs.h_scale,
                0.0,
                0.0,
                self.gs.font_size,
                0.0,
                self.gs.text_rise,
            );
            let transform = scale.then(&self.text_matrix).then(&self.gs.ctm);

            let (unicode, width_per_mille, advance_is_estimated, outline) = match &font {
                Some(f) => {
                    let (u, w) = f.decode_simple(code);
                    (u, w, false, f.glyph_outline(code, u))
                }
                None => (None, PLACEHOLDER_GLYPH_WIDTH_PER_MILLE, true, None),
            };

            self.display.items.push(DisplayItem::Glyph {
                font: font_name.clone(),
                code: code as u32,
                unicode,
                transform,
                color: self.gs.fill_color,
                advance_is_estimated,
                outline,
            });

            let word_spacing = if code == b' ' {
                self.gs.word_spacing
            } else {
                0.0
            };
            let advance = (width_per_mille / 1000.0 * self.gs.font_size
                + self.gs.char_spacing
                + word_spacing)
                * self.gs.h_scale;
            self.text_matrix = Matrix::translation(advance, 0.0).then(&self.text_matrix);
        }
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
