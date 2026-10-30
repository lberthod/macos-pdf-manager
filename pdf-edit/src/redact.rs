//! Caviardage sécurisé — `ROADMAP_IMPROVEMENT.md` §1.2. Contrairement à
//! [`crate::EditSession::replace_text_with_overlay`] ("6b" : masque +
//! redessine sans jamais toucher le flux de contenu d'origine, documenté là
//! comme non sécurisé pour un usage de caviardage), `redact_content_stream`
//! retire réellement les opérateurs de peinture (texte, chemins, images et
//! formulaires `XObject`) dont la zone occupée intersecte le rectangle à
//! caviarder, avant qu'un rectangle plein ne soit redessiné par-dessus.
//!
//! **Portée de cette première passe** (voir `ROADMAP_IMPROVEMENT.md` pour le
//! détail et les suites envisagées) :
//! - Un état graphique minimal est suivi ici en miroir de
//!   `pdf_core::interp::Interpreter` (pile `q`/`Q`, CTM, matrices de texte) —
//!   dupliqué plutôt que réutilisé : l'interpréteur ne garde aucun lien entre
//!   un `DisplayItem` émis et l'instruction de flux qui l'a produit (il en
//!   faudrait un pour décider quoi retirer), et un `XObject /Form` peut
//!   émettre un nombre variable d'items imbriqués, rendant une corrélation
//!   par simple comptage d'items peu fiable.
//! - Un `Do` (image ou formulaire) dont la zone occupée croise la zone à
//!   caviarder est retiré **en entier**, sans jamais descendre dedans — pas
//!   de caviardage partiel à l'intérieur d'une image ou d'un formulaire. Plus
//!   sûr par excès (sur-suppression) que par défaut.
//! - Les images en ligne (`BI`...`ID`...`EI`) ne sont pas prises en charge :
//!   leur présence sur la page fait échouer l'opération avec un message
//!   explicite plutôt que de risquer de laisser du contenu non vérifié.
//! - Le clip (`W`/`W*`) n'est pas pris en compte dans la décision de retrait :
//!   un opérateur dont la zone *nominale* croise la cible est retiré même
//!   s'il est en réalité masqué par un clip plus étroit — même logique de
//!   prudence par sur-suppression.

use pdf_core::content::{parse_content_stream, ContentInstruction};
use pdf_core::display::Matrix;
use pdf_core::document::Document;
use pdf_core::font::Font;
use pdf_core::object::{Dictionary, Object};
use pdf_core::writer::write_object;
use std::collections::{HashMap, HashSet};

/// Rectangle en espace page, coins déjà normalisés (`x0 <= x1`, `y0 <= y1`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Rect {
    pub(crate) fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Self {
            x0: x0.min(x1),
            y0: y0.min(y1),
            x1: x0.max(x1),
            y1: y0.max(y1),
        }
    }

    /// Intersection stricte (un simple contact sur un bord ne compte pas —
    /// évite de retirer un voisin immédiat par un souci d'arrondi flottant).
    pub(crate) fn intersects(&self, other: &Rect) -> bool {
        self.x0 < other.x1 && other.x0 < self.x1 && self.y0 < other.y1 && other.y0 < self.y1
    }

    fn from_points(points: &[(f64, f64)]) -> Option<Rect> {
        let mut it = points.iter().copied();
        let (fx, fy) = it.next()?;
        let mut r = Rect::new(fx, fy, fx, fy);
        for (x, y) in it {
            r.x0 = r.x0.min(x);
            r.y0 = r.y0.min(y);
            r.x1 = r.x1.max(x);
            r.y1 = r.y1.max(y);
        }
        Some(r)
    }
}

/// Un bloc de contenu marqué (`BDC`/`BMC` … `EMC`) en cours d'imbrication.
/// `has_actual_text_like_key` est vrai si son dictionnaire de propriétés
/// porte `/ActualText`, `/Alt`, `/E` ou `/Lang` (ISO 32000-1 §14.9.3, §14.9.4) :
/// ces clés portent une représentation textuelle du contenu marqué qui existe
/// **indépendamment** des opérateurs de peinture qu'il contient (accessibilité,
/// OCR taggé) — retirer l'opérateur de peinture retiré ne suffit donc pas, il
/// faut retirer le bloc entier. `any_removed` accumule si un opérateur de
/// peinture a été retiré n'importe où à l'intérieur (y compris dans un bloc
/// imbriqué), pour décider à `EMC` s'il faut retirer tout le bloc.
struct MarkedContentFrame {
    start_idx: usize,
    has_actual_text_like_key: bool,
    any_removed: bool,
}

#[derive(Clone)]
struct GState {
    ctm: Matrix,
    font: Option<String>,
    font_size: f64,
    char_spacing: f64,
    word_spacing: f64,
    h_scale: f64,
    leading: f64,
}

impl Default for GState {
    fn default() -> Self {
        Self {
            ctm: Matrix::IDENTITY,
            font: None,
            font_size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            h_scale: 1.0,
            leading: 0.0,
        }
    }
}

fn num(ops: &[Object], i: usize) -> f64 {
    match ops.get(i) {
        Some(Object::Integer(n)) => *n as f64,
        Some(Object::Real(f)) => *f,
        _ => 0.0,
    }
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

/// Avance (espace texte, avant `text_matrix`/CTM) d'une chaîne montrée par
/// `Tj`/`TJ`/`'`/`"`, en miroir exact de `Interpreter::emit_glyph` — mais on
/// n'a besoin ici que de l'avance totale, pas de la position de chaque
/// glyphe individuellement.
fn text_run_advance(bytes: &[u8], font: Option<&Font>, gs: &GState) -> f64 {
    let mut advance = 0.0;
    match font {
        Some(f) if f.is_composite() => {
            for cid in f.decode_composite(bytes) {
                let (_, width_per_mille) = f.cid_metrics(cid);
                advance += width_per_mille / 1000.0 * gs.font_size + gs.char_spacing;
            }
        }
        Some(f) => {
            for &code in bytes {
                let (_, width_per_mille) = f.decode_simple(code);
                let word_spacing = if code == b' ' { gs.word_spacing } else { 0.0 };
                advance += width_per_mille / 1000.0 * gs.font_size + gs.char_spacing + word_spacing;
            }
        }
        None => {
            const PLACEHOLDER_WIDTH_PER_MILLE: f64 = 500.0;
            for &code in bytes {
                let word_spacing = if code == b' ' { gs.word_spacing } else { 0.0 };
                advance += PLACEHOLDER_WIDTH_PER_MILLE / 1000.0 * gs.font_size
                    + gs.char_spacing
                    + word_spacing;
            }
        }
    }
    advance * gs.h_scale
}

/// Boîte englobante (espace page) d'une chaîne montrée, en partant de
/// `text_matrix` courant — généreuse en hauteur (0.25 em sous la ligne de
/// base, 0.9 em au-dessus) plutôt que la vraie boîte de chaque glyphe :
/// erreur volontairement du côté de la sur-suppression.
fn text_run_bbox(width: f64, gs: &GState, text_matrix: &Matrix) -> Rect {
    let m = text_matrix.then(&gs.ctm);
    let fs = gs.font_size.max(0.0);
    let corners = [
        m.apply(0.0, -0.25 * fs),
        m.apply(width, -0.25 * fs),
        m.apply(width, 0.9 * fs),
        m.apply(0.0, 0.9 * fs),
    ];
    Rect::from_points(&corners).unwrap_or(Rect::new(m.e, m.f, m.e, m.f))
}

fn lookup_xobject(doc: &Document, resources: &Dictionary, name: &str) -> Option<Object> {
    let cat_obj = resources.get("XObject")?;
    let cat_dict = doc.get(cat_obj).ok()?;
    let cat_dict = cat_dict.as_dict()?;
    let entry = cat_dict.get(name)?;
    doc.get(entry).ok()
}

fn xobject_bbox(doc: &Document, resources: &Dictionary, name: &str, ctm: &Matrix) -> Option<Rect> {
    let xobj = lookup_xobject(doc, resources, name)?;
    let Object::Stream(stream) = &xobj else {
        return None;
    };
    match stream.dict.get("Subtype").and_then(|o| o.as_name()) {
        Some("Image") => {
            let corners = [
                ctm.apply(0.0, 0.0),
                ctm.apply(1.0, 0.0),
                ctm.apply(1.0, 1.0),
                ctm.apply(0.0, 1.0),
            ];
            Rect::from_points(&corners)
        }
        Some("Form") => {
            let bbox = stream.dict.get("BBox").and_then(|o| o.as_array())?;
            if bbox.len() != 4 {
                return None;
            }
            let (x0, y0, x1, y1) = (
                bbox[0].as_int().map(|n| n as f64).unwrap_or(0.0),
                bbox[1].as_int().map(|n| n as f64).unwrap_or(0.0),
                bbox[2].as_int().map(|n| n as f64).unwrap_or(0.0),
                bbox[3].as_int().map(|n| n as f64).unwrap_or(0.0),
            );
            let form_matrix = stream
                .dict
                .get("Matrix")
                .and_then(|o| o.as_array())
                .and_then(matrix_from)
                .unwrap_or(Matrix::IDENTITY);
            let effective = form_matrix.then(ctm);
            let corners = [
                effective.apply(x0, y0),
                effective.apply(x1, y0),
                effective.apply(x1, y1),
                effective.apply(x0, y1),
            ];
            Rect::from_points(&corners)
        }
        _ => None,
    }
}

fn resolve_font(
    doc: &Document,
    resources: &Dictionary,
    cache: &mut HashMap<String, Option<std::rc::Rc<Font>>>,
    name: &str,
) -> Option<std::rc::Rc<Font>> {
    if let Some(cached) = cache.get(name) {
        return cached.clone();
    }
    let font = (|| {
        let cat_obj = resources.get("Font")?;
        let cat_dict = doc.get(cat_obj).ok()?;
        let cat_dict = cat_dict.as_dict()?;
        let entry = cat_dict.get(name)?;
        let dict = doc.get(entry).ok()?;
        let dict = dict.as_dict()?;
        Font::load(doc, dict).ok().map(std::rc::Rc::new)
    })();
    cache.insert(name.to_string(), font.clone());
    font
}

/// Dictionnaire de propriétés d'un `BDC /Tag <props> BDC` — `<props>` est
/// soit un dictionnaire inline, soit un `Name` qui référence une entrée de
/// `/Properties` dans les ressources de la page (ISO 32000-1 §14.6.2).
fn marked_content_properties(
    doc: &Document,
    resources: &Dictionary,
    ops: &[Object],
) -> Option<Dictionary> {
    match ops.get(1)? {
        Object::Dictionary(d) => Some(d.clone()),
        Object::Name(name) => {
            let cat_obj = resources.get("Properties")?;
            let cat_dict = doc.get(cat_obj).ok()?;
            let cat_dict = cat_dict.as_dict()?;
            let entry = cat_dict.get(name)?;
            doc.get(entry).ok()?.as_dict().cloned()
        }
        _ => None,
    }
}

/// Vrai si `dict` porte une des clés qui font qu'un bloc de contenu marqué
/// ne peut pas être réduit à ses seuls opérateurs de peinture (voir
/// `MarkedContentFrame`).
fn has_actual_text_like_key(dict: &Dictionary) -> bool {
    ["ActualText", "Alt", "E", "Lang"]
        .iter()
        .any(|key| dict.get(key).is_some())
}

/// Retire du flux de contenu décodé `content` tout opérateur de peinture
/// (texte, chemin, image/formulaire `XObject`) dont la zone occupée
/// intersecte `target` (espace page), et reconstruit un flux valide avec les
/// instructions restantes. Renvoie `(nouveau flux, au moins un retrait
/// effectué)`.
pub(crate) fn redact_content_stream(
    doc: &Document,
    content: &[u8],
    page_resources: &Dictionary,
    target: Rect,
) -> Result<(Vec<u8>, bool), String> {
    let instructions = parse_content_stream(content).map_err(|e| e.to_string())?;

    // Le flux est déjà tokenisé à ce stade (`BI` est une instruction à part
    // entière, voir `parse_content_stream`) : plus besoin d'un scan d'octets
    // bruts, qui donnait de faux positifs sur `(BIENVENUE) Tj`, `/GS0BI gs`,
    // ou tout flux binaire de police Type3 contenant la séquence `BI` par
    // hasard (AUDIT_FIXES_2026-09-03.md #16). Les images en ligne ne sont
    // toujours pas prises en charge : leur présence réelle fait échouer
    // l'opération avec un message explicite plutôt que de risquer de laisser
    // du contenu non vérifié.
    if instructions.iter().any(|i| i.operator == "BI") {
        return Err(
            "cette page contient des images en ligne (BI/ID/EI) : le caviardage ne les prend pas \
             encore en charge, opération refusée par prudence plutôt que de risquer de laisser du \
             contenu non vérifié"
                .to_string(),
        );
    }

    let mut gs = GState::default();
    let mut gs_stack: Vec<GState> = Vec::new();
    let mut text_matrix = Matrix::IDENTITY;
    let mut text_line_matrix = Matrix::IDENTITY;
    let mut font_cache: HashMap<String, Option<std::rc::Rc<Font>>> = HashMap::new();

    // Chemin en cours de construction (`m`/`l`/`c`/`v`/`y`/`re` depuis le
    // dernier opérateur de peinture), déjà transformé en espace page.
    // `path_instr_indices` retient quelles instructions ont construit ce
    // chemin, pour pouvoir les retirer elles aussi (pas seulement
    // l'opérateur de peinture) si le chemin est jugé à caviarder — sinon
    // les coordonnées du chemin resteraient dans le flux, orphelines mais
    // présentes.
    let mut path_points: Vec<(f64, f64)> = Vec::new();
    let mut path_instr_indices: Vec<usize> = Vec::new();
    let mut subpath_start = (0.0, 0.0);

    let mut keep = vec![true; instructions.len()];
    let mut redacted_anything = false;
    let mut mc_stack: Vec<MarkedContentFrame> = Vec::new();

    let show_text = |bytes: &[u8],
                     gs: &GState,
                     text_matrix: &mut Matrix,
                     font_cache: &mut HashMap<String, Option<std::rc::Rc<Font>>>|
     -> bool {
        let font = gs
            .font
            .as_deref()
            .and_then(|name| resolve_font(doc, page_resources, font_cache, name));
        let width = text_run_advance(bytes, font.as_deref(), gs);
        let bbox = text_run_bbox(width, gs, text_matrix);
        *text_matrix = Matrix::translation(width, 0.0).then(text_matrix);
        target.intersects(&bbox)
    };

    for (idx, instr) in instructions.iter().enumerate() {
        let ops = &instr.operands;
        match instr.operator.as_str() {
            "q" => gs_stack.push(gs.clone()),
            "Q" => {
                if let Some(prev) = gs_stack.pop() {
                    gs = prev;
                }
            }
            "cm" => {
                if let Some(m) = matrix_from(ops) {
                    gs.ctm = m.then(&gs.ctm);
                }
            }
            "BT" => {
                text_matrix = Matrix::IDENTITY;
                text_line_matrix = Matrix::IDENTITY;
            }
            "Tf" => {
                if let Some(Object::Name(name)) = ops.first() {
                    gs.font = Some(name.clone());
                }
                gs.font_size = num(ops, 1);
            }
            "Tc" => gs.char_spacing = num(ops, 0),
            "Tw" => gs.word_spacing = num(ops, 0),
            "Tz" => gs.h_scale = num(ops, 0) / 100.0,
            "TL" => gs.leading = num(ops, 0),
            "Td" => {
                let m = Matrix::translation(num(ops, 0), num(ops, 1)).then(&text_line_matrix);
                text_line_matrix = m;
                text_matrix = m;
            }
            "TD" => {
                gs.leading = -num(ops, 1);
                let m = Matrix::translation(num(ops, 0), num(ops, 1)).then(&text_line_matrix);
                text_line_matrix = m;
                text_matrix = m;
            }
            "Tm" => {
                let m = matrix_from(ops).unwrap_or(Matrix::IDENTITY);
                text_line_matrix = m;
                text_matrix = m;
            }
            "T*" => {
                let m = Matrix::translation(0.0, -gs.leading).then(&text_line_matrix);
                text_line_matrix = m;
                text_matrix = m;
            }
            "Tj" => {
                if let Some(Object::String(s)) = ops.first() {
                    if show_text(s, &gs, &mut text_matrix, &mut font_cache) {
                        keep[idx] = false;
                        redacted_anything = true;
                        for frame in mc_stack.iter_mut() {
                            frame.any_removed = true;
                        }
                    }
                }
            }
            "'" => {
                let m = Matrix::translation(0.0, -gs.leading).then(&text_line_matrix);
                text_line_matrix = m;
                text_matrix = m;
                if let Some(Object::String(s)) = ops.first() {
                    if show_text(s, &gs, &mut text_matrix, &mut font_cache) {
                        keep[idx] = false;
                        redacted_anything = true;
                        for frame in mc_stack.iter_mut() {
                            frame.any_removed = true;
                        }
                    }
                }
            }
            "\"" => {
                gs.word_spacing = num(ops, 0);
                gs.char_spacing = num(ops, 1);
                let m = Matrix::translation(0.0, -gs.leading).then(&text_line_matrix);
                text_line_matrix = m;
                text_matrix = m;
                if let Some(Object::String(s)) = ops.get(2) {
                    if show_text(s, &gs, &mut text_matrix, &mut font_cache) {
                        keep[idx] = false;
                        redacted_anything = true;
                        for frame in mc_stack.iter_mut() {
                            frame.any_removed = true;
                        }
                    }
                }
            }
            "TJ" => {
                if let Some(Object::Array(items)) = ops.first() {
                    let mut any_hit = false;
                    for item in items {
                        match item {
                            Object::String(s) => {
                                if show_text(s, &gs, &mut text_matrix, &mut font_cache) {
                                    any_hit = true;
                                }
                            }
                            other => {
                                if let Some(adj) =
                                    other.as_int().map(|n| n as f64).or(match other {
                                        Object::Real(f) => Some(*f),
                                        _ => None,
                                    })
                                {
                                    let dx = -adj / 1000.0 * gs.font_size * gs.h_scale;
                                    text_matrix = Matrix::translation(dx, 0.0).then(&text_matrix);
                                }
                            }
                        }
                    }
                    if any_hit {
                        keep[idx] = false;
                        redacted_anything = true;
                        for frame in mc_stack.iter_mut() {
                            frame.any_removed = true;
                        }
                    }
                }
            }
            "m" => {
                let p = gs.ctm.apply(num(ops, 0), num(ops, 1));
                subpath_start = p;
                path_points.push(p);
                path_instr_indices.push(idx);
            }
            "l" => {
                path_points.push(gs.ctm.apply(num(ops, 0), num(ops, 1)));
                path_instr_indices.push(idx);
            }
            "c" => {
                path_points.push(gs.ctm.apply(num(ops, 0), num(ops, 1)));
                path_points.push(gs.ctm.apply(num(ops, 2), num(ops, 3)));
                path_points.push(gs.ctm.apply(num(ops, 4), num(ops, 5)));
                path_instr_indices.push(idx);
            }
            "v" | "y" => {
                path_points.push(gs.ctm.apply(num(ops, 0), num(ops, 1)));
                path_points.push(gs.ctm.apply(num(ops, 2), num(ops, 3)));
                path_instr_indices.push(idx);
            }
            "h" => {
                path_points.push(subpath_start);
                path_instr_indices.push(idx);
            }
            "re" => {
                let (x, y, w, h) = (num(ops, 0), num(ops, 1), num(ops, 2), num(ops, 3));
                for (px, py) in [(x, y), (x + w, y), (x + w, y + h), (x, y + h)] {
                    path_points.push(gs.ctm.apply(px, py));
                }
                subpath_start = gs.ctm.apply(x, y);
                path_instr_indices.push(idx);
            }
            // Le `W`/`W*` fait partie du chemin : si un chemin *peint* est
            // retiré, son `W` part avec lui plutôt que de rester orphelin.
            "W" | "W*" => path_instr_indices.push(idx),
            // Audit 2026-10-26 #24 : `n` termine un chemin **sans le peindre**
            // (en pratique `re W n`, un pur rognage). Il était traité comme une
            // peinture : un clip croisant la zone perdait son tracé et son `n`
            // (en laissant un `W` orphelin), et le contenu qu'il rognait (photo
            // recadrée, texte débordant) apparaissait sur toute la page, alors
            // que rien de visible n'avait été retiré. Il est conservé intact.
            "n" => {
                path_points.clear();
                path_instr_indices.clear();
            }
            "f" | "F" | "f*" | "S" | "s" | "B" | "B*" | "b" | "b*" => {
                if let Some(bbox) = Rect::from_points(&path_points) {
                    if target.intersects(&bbox) {
                        keep[idx] = false;
                        for &path_idx in &path_instr_indices {
                            keep[path_idx] = false;
                        }
                        redacted_anything = true;
                        for frame in mc_stack.iter_mut() {
                            frame.any_removed = true;
                        }
                    }
                }
                path_points.clear();
                path_instr_indices.clear();
            }
            "Do" => {
                if let Some(Object::Name(name)) = ops.first() {
                    match xobject_bbox(doc, page_resources, name, &gs.ctm) {
                        Some(bbox) => {
                            if target.intersects(&bbox) {
                                keep[idx] = false;
                                redacted_anything = true;
                                for frame in mc_stack.iter_mut() {
                                    frame.any_removed = true;
                                }
                            }
                        }
                        // XObject introuvable, `/BBox` absent ou `/Subtype`
                        // inattendu : impossible de savoir si sa zone
                        // occupée croise `target`. Le conserver serait
                        // fail-open (AUDIT_FIXES_2026-09-03.md #4) — plus
                        // sûr de refuser l'opération entière que de risquer
                        // de laisser du contenu non vérifié dans la sortie.
                        None => {
                            return Err(format!(
                                "XObject /{name} : boîte inconnue, caviardage refusé"
                            ))
                        }
                    }
                }
            }
            "sh" => {
                // Peint le clip courant avec un ombrage (ISO 32000-1
                // §8.7.4.3) : sans suivi du clip (voir la doc de module),
                // impossible de calculer sa zone occupée réelle. Traité
                // comme couvrant toute la page — donc toujours retiré dès
                // que `target` est non vide, cohérent avec la sur-suppression
                // assumée pour `Do` ci-dessus (AUDIT_FIXES_2026-09-03.md #4).
                keep[idx] = false;
                redacted_anything = true;
                for frame in mc_stack.iter_mut() {
                    frame.any_removed = true;
                }
            }
            "BDC" => {
                let has_actual_text_like_key = marked_content_properties(doc, page_resources, ops)
                    .is_some_and(|props| has_actual_text_like_key(&props));
                mc_stack.push(MarkedContentFrame {
                    start_idx: idx,
                    has_actual_text_like_key,
                    any_removed: false,
                });
            }
            "BMC" => mc_stack.push(MarkedContentFrame {
                start_idx: idx,
                has_actual_text_like_key: false,
                any_removed: false,
            }),
            "EMC" => {
                if let Some(frame) = mc_stack.pop() {
                    if frame.has_actual_text_like_key && frame.any_removed {
                        keep[frame.start_idx..=idx].fill(false);
                        redacted_anything = true;
                        if let Some(parent) = mc_stack.last_mut() {
                            parent.any_removed = true;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let mut out = Vec::new();
    for (idx, instr) in instructions.iter().enumerate() {
        if !keep[idx] {
            continue;
        }
        write_instruction(instr, &mut out);
    }
    Ok((out, redacted_anything))
}

/// Noms des XObjects encore dessinés par un `Do` dans le flux `content` —
/// sert à retirer des ressources de la page ceux dont tous les `Do` ont été
/// caviardés (audit 2026-10-26 #5).
pub(crate) fn xobject_names_used(content: &[u8]) -> Result<HashSet<String>, String> {
    let instructions = parse_content_stream(content).map_err(|e| e.to_string())?;
    Ok(instructions
        .iter()
        .filter(|instr| instr.operator == "Do")
        .filter_map(|instr| match instr.operands.first() {
            Some(Object::Name(name)) => Some(name.clone()),
            _ => None,
        })
        .collect())
}

fn write_instruction(instr: &ContentInstruction, out: &mut Vec<u8>) {
    for operand in &instr.operands {
        write_object(operand, out);
        out.push(b' ');
    }
    out.extend_from_slice(instr.operator.as_bytes());
    out.push(b'\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdf_core::object::Dictionary;

    fn empty_doc() -> Document {
        // Un document minimal juste pour satisfaire la signature — aucune
        // des zones de test ci-dessous ne référence de police/XObject par
        // objet indirect, donc `resolve` n'est jamais appelé.
        let body = concat!(
            "%PDF-1.7\n",
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
            "2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n",
        );
        let mut bytes = body.as_bytes().to_vec();
        let off1 = 9;
        let off2 = bytes.windows(7).position(|w| w == b"2 0 obj").unwrap();
        let xref_offset = bytes.len();
        let xref = format!(
            "xref\n0 3\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \ntrailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF"
        );
        bytes.extend_from_slice(xref.as_bytes());
        Document::open(bytes).unwrap()
    }

    #[test]
    fn removes_a_filled_rectangle_intersecting_the_target() {
        let doc = empty_doc();
        let content = b"0 0 0 rg\n100 100 50 50 re f\n";
        let target = Rect::new(90.0, 90.0, 200.0, 200.0);
        let (out, redacted) =
            redact_content_stream(&doc, content, &Dictionary::new(), target).unwrap();
        assert!(redacted);
        assert!(!out.windows(2).any(|w| w == b"re"));
    }

    #[test]
    fn keeps_a_rectangle_outside_the_target() {
        let doc = empty_doc();
        let content = b"0 0 0 rg\n100 100 50 50 re f\n";
        let target = Rect::new(500.0, 500.0, 600.0, 600.0);
        let (out, redacted) =
            redact_content_stream(&doc, content, &Dictionary::new(), target).unwrap();
        assert!(!redacted);
        assert!(out.windows(2).any(|w| w == b"re"));
    }

    #[test]
    fn removes_text_positioned_inside_the_target() {
        let doc = empty_doc();
        let content = b"BT /F1 12 Tf 100 100 Td (secret) Tj ET";
        let target = Rect::new(90.0, 90.0, 300.0, 120.0);
        let (out, redacted) =
            redact_content_stream(&doc, content, &Dictionary::new(), target).unwrap();
        assert!(redacted);
        assert!(!out.windows(2).any(|w| w == b"Tj"));
    }

    #[test]
    fn keeps_text_positioned_outside_the_target() {
        let doc = empty_doc();
        let content = b"BT /F1 12 Tf 100 100 Td (visible) Tj ET";
        let target = Rect::new(500.0, 500.0, 600.0, 600.0);
        let (out, redacted) =
            redact_content_stream(&doc, content, &Dictionary::new(), target).unwrap();
        assert!(!redacted);
        assert!(out.windows(2).any(|w| w == b"Tj"));
    }

    #[test]
    fn refuses_pages_with_inline_images() {
        let doc = empty_doc();
        let content = b"BI /W 1 /H 1 /BPC 8 /CS /G ID \xff EI";
        let target = Rect::new(0.0, 0.0, 10.0, 10.0);
        let err = redact_content_stream(&doc, content, &Dictionary::new(), target).unwrap_err();
        assert!(err.contains("images en ligne"));
    }

    /// AUDIT_FIXES_2026-09-03.md #4 : un `Tj` retiré à l'intérieur d'un bloc
    /// de contenu marqué `/ActualText` doit faire retirer le bloc entier
    /// (`BDC`..`EMC`), sinon la valeur `/ActualText` — qui porte le vrai
    /// texte indépendamment du glyphe affiché — resterait dans le flux.
    #[test]
    fn removes_the_whole_marked_content_block_when_its_actual_text_leaks() {
        let doc = empty_doc();
        let content = b"BT /F1 12 Tf 100 100 Td /Span << /ActualText (secret) >> BDC (x) Tj EMC ET";
        let target = Rect::new(90.0, 90.0, 300.0, 120.0);
        let (out, redacted) =
            redact_content_stream(&doc, content, &Dictionary::new(), target).unwrap();
        assert!(redacted);
        assert!(!out.windows(6).any(|w| w == b"secret"));
        assert!(!out.windows(2).any(|w| w == b"Tj"));
    }

    /// AUDIT_FIXES_2026-09-03.md #4 : `sh` (ombrage sur le clip courant) n'a
    /// pas de zone occupée calculable sans suivi du clip — toujours retiré
    /// plutôt que conservé par défaut (fail-open).
    #[test]
    fn removes_shading_operator_by_default() {
        let doc = empty_doc();
        let content = b"q 0 0 100 100 re W n /Sh0 sh Q";
        let target = Rect::new(0.0, 0.0, 10.0, 10.0);
        let (out, redacted) =
            redact_content_stream(&doc, content, &Dictionary::new(), target).unwrap();
        assert!(redacted);
        assert!(!out.windows(2).any(|w| w == b"sh"));
    }

    /// AUDIT_FIXES_2026-09-03.md #4 : un `Do` dont `xobject_bbox` ne peut pas
    /// être déterminé (ressource introuvable) doit faire échouer l'opération,
    /// pas la laisser passer silencieusement (fail-open).
    #[test]
    fn rejects_a_do_operator_with_unknown_xobject() {
        let doc = empty_doc();
        let content = b"/Missing Do";
        let target = Rect::new(0.0, 0.0, 10.0, 10.0);
        let err = redact_content_stream(&doc, content, &Dictionary::new(), target).unwrap_err();
        assert!(err.contains("Missing"));
    }

    /// AUDIT_FIXES_2026-09-03.md #16 : la séquence d'octets "BI" apparaissant
    /// dans une chaîne de texte (ou tout autre contexte qui n'est pas
    /// l'opérateur `BI` d'une vraie image inline) ne doit plus faire échouer
    /// le caviardage — seule une véritable instruction `BI` doit le faire
    /// (voir `refuses_pages_with_inline_images` ci-dessus).
    #[test]
    fn does_not_reject_the_literal_bytes_bi_inside_unrelated_content() {
        let doc = empty_doc();
        let content = b"BT /F1 12 Tf 700 700 Td (BIENVENUE) Tj ET";
        let target = Rect::new(0.0, 0.0, 10.0, 10.0); // hors zone du texte
        let (out, redacted) =
            redact_content_stream(&doc, content, &Dictionary::new(), target).unwrap();
        assert!(!redacted);
        assert!(out.windows(2).any(|w| w == b"Tj"));
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    /// Audit 2026-10-26 #24 : un pur rognage (`re W n`) croisant la zone était
    /// retiré comme une peinture. Le clip disparaissait (avec un `W` orphelin)
    /// et le contenu qu'il rognait apparaissait sur toute la page, alors que
    /// rien de visible n'avait été retiré.
    #[test]
    fn keeps_a_clip_only_path_crossing_the_target() {
        let doc = empty_doc();
        let content = b"q\n10 10 100 100 re W n\n500 500 10 10 re f\nQ\n";
        let target = Rect::new(0.0, 0.0, 200.0, 200.0);
        let (out, redacted) =
            redact_content_stream(&doc, content, &Dictionary::new(), target).unwrap();
        assert!(!redacted, "rien de visible dans la zone : rien à retirer");
        assert!(
            contains(&out, b"10 10 100 100 re\nW\nn\n"),
            "le clip doit rester intact : {}",
            String::from_utf8_lossy(&out)
        );
    }

    /// Un chemin à la fois peint et utilisé comme clip (`re W f`) est bien
    /// retiré, et son `W` avec lui plutôt que de rester orphelin.
    #[test]
    fn removes_a_painted_clip_path_together_with_its_w() {
        let doc = empty_doc();
        let content = b"10 10 100 100 re W f\n";
        let target = Rect::new(0.0, 0.0, 200.0, 200.0);
        let (out, redacted) =
            redact_content_stream(&doc, content, &Dictionary::new(), target).unwrap();
        assert!(redacted);
        assert!(
            out.iter().all(|b| b.is_ascii_whitespace()),
            "tracé, `W` et peinture doivent tous partir : {}",
            String::from_utf8_lossy(&out)
        );
    }
}
