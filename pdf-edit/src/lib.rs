//! Journal d'opérations d'édition (undo/redo, sauvegarde incrémentale) —
//! Sprint 13-14 (voir sprint.md). Construit sur trois briques de
//! `pdf-core` ajoutées pour ce sprint : `writer` (sérialisation d'`Object`),
//! `Document::save_incremental` (ajout en fin de fichier + xref chaînée),
//! et `Interpreter::run_page_with_annotations` (rendu des `/AP /N`).
//!
//! `EditSession` porte un `Document` en lecture seule plus un ensemble
//! d'objets *en attente* (`pending`) — nouveaux ou mise à jour d'objets
//! existants — jamais écrits sur disque tant que `save_as` n'est pas
//! appelé. Chaque opération sémantique (ajouter une annotation, remplir un
//! champ de formulaire) empile un [`EditOp`] qui capture l'état "avant" de
//! chaque objet existant qu'elle modifie, pour permettre un `undo` exact ;
//! les nouveaux objets qu'elle crée (annotation, flux d'apparence, police)
//! restent dans `pending` même après un `undo` — un `undo` d'ajout
//! d'annotation la rend orpheline (plus référencée depuis `/Annots`, donc
//! invisible et jamais visitée), il ne la supprime pas physiquement.
//! Nettoyer les objets orphelins est un problème distinct (garbage
//! collection, Sprint 15-16 dans sprint.md), pas traité ici.
//!
//! **Non fait dans cette passe** (voir sprint.md Sprint 13-14) :
//! - Pas d'interface `pdf-ui` pour dessiner une annotation ou cliquer un
//!   champ de formulaire — seule l'API `pdf-edit` existe, testée
//!   directement et via `pdf-cli`. Le câblage UI (outil de surlignage à la
//!   souris, édition de champ au clic) est un chantier UX séparé.
//! - Pas de transparence réelle (`/ca`, ExtGState) sur les surlignages :
//!   `pdf-core::interp` ne gère que `/LW` dans `gs` pour l'instant (voir sa
//!   doc de module) — un surlignage est donc rendu en couleur pleine, pas
//!   semi-transparente comme le ferait un vrai lecteur.
//! - Un seul niveau de nom de champ (`/T` direct) est résolu pour
//!   `set_form_field_value` — pas de noms qualifiés par `/Parent`
//!   (`"parent.enfant"`), rare en dehors de formulaires très structurés.
//! - Signatures numériques, cases à cocher/boutons radio, formes/lignes
//!   libres : non faits (voir sprint.md, portée progressive prévue au-delà
//!   de ce sprint).

use pdf_core::document::Document;
use pdf_core::object::{Dictionary, ObjRef, Object, Stream};
use std::path::Path;

/// Une opération d'édition réversible : capture, pour chaque objet
/// **existant** modifié, sa valeur avant (`undo`) et après (`redo`)
/// l'opération. Les objets nouvellement créés par l'opération ne sont pas
/// listés ici (voir la doc de module).
#[derive(Debug, Clone)]
struct EditOp {
    modified: Vec<(u32, Object, Object)>,
}

pub struct EditSession {
    doc: Document,
    /// Objets nouveaux ou mis à jour, pas encore écrits sur disque —
    /// numéro d'objet -> valeur courante (génération toujours 0 : ce moteur
    /// ne gère pas la réutilisation de numéros de générations précédentes).
    pending: std::collections::BTreeMap<u32, Object>,
    next_num: u32,
    undo_stack: Vec<EditOp>,
    redo_stack: Vec<EditOp>,
}

impl EditSession {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        let data = std::fs::read(path.as_ref()).map_err(|e| e.to_string())?;
        let doc = Document::open(data).map_err(|e| e.to_string())?;
        let next_num = doc.next_free_object_num();
        Ok(Self {
            doc,
            pending: std::collections::BTreeMap::new(),
            next_num,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        })
    }

    /// Accès en lecture au document sous-jacent — pour inspecter l'état
    /// courant (déjà modifié ou non) sans passer par `pdf-app`.
    pub fn document(&self) -> &Document {
        &self.doc
    }

    fn alloc_num(&mut self) -> u32 {
        let num = self.next_num;
        self.next_num += 1;
        num
    }

    /// Valeur courante d'un objet : dans `pending` s'il a déjà été touché
    /// par cette session, sinon résolu depuis le document original.
    fn current(&self, num: u32) -> Result<Object, String> {
        if let Some(obj) = self.pending.get(&num) {
            return Ok(obj.clone());
        }
        self.doc
            .resolve(ObjRef::new(num, 0))
            .map_err(|e| e.to_string())
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        let Some(op) = self.undo_stack.pop() else {
            return false;
        };
        for (num, before, _after) in &op.modified {
            self.pending.insert(*num, before.clone());
        }
        self.redo_stack.push(op);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(op) = self.redo_stack.pop() else {
            return false;
        };
        for (num, _before, after) in &op.modified {
            self.pending.insert(*num, after.clone());
        }
        self.undo_stack.push(op);
        true
    }

    fn commit(&mut self, op: EditOp) {
        for (num, _before, after) in &op.modified {
            self.pending.insert(*num, after.clone());
        }
        self.undo_stack.push(op);
        self.redo_stack.clear();
    }

    /// Ajoute une annotation `/Highlight` couvrant `rect` (`[x0 y0 x1 y1]`,
    /// espace page) sur la page `page_index`, avec un flux d'apparence
    /// (`/AP /N`) qui remplit ce rectangle de `color` (RGB 0.0-1.0) — voir
    /// la doc de module pour la limitation "pas de vraie transparence".
    /// `quad_points` (ISO 32000-1 §12.5.6.10, 8 nombres = 4 sommets, sens
    /// direct) : si vide, dérivé automatiquement des quatre coins de `rect`.
    pub fn add_highlight_annotation(
        &mut self,
        page_index: usize,
        rect: [f64; 4],
        color: (f32, f32, f32),
        quad_points: Vec<f64>,
    ) -> Result<(), String> {
        let page = self.doc.page(page_index).map_err(|e| e.to_string())?;
        let page_ref = page
            .object_ref
            .ok_or_else(|| "page has no indirect object reference".to_string())?;

        let (x0, y0, x1, y1) = (
            rect[0].min(rect[2]),
            rect[1].min(rect[3]),
            rect[0].max(rect[2]),
            rect[1].max(rect[3]),
        );
        let width = (x1 - x0).max(1.0);
        let height = (y1 - y0).max(1.0);

        let quad = if quad_points.len() == 8 {
            quad_points
        } else {
            // Sens direct ISO 32000-1 §12.5.6.10 : coin haut-gauche, haut-droit,
            // bas-gauche, bas-droit.
            vec![x0, y1, x1, y1, x0, y0, x1, y0]
        };

        let ap_num = self.alloc_num();
        let ap_content = format!(
            "{:.3} {:.3} {:.3} rg 0 0 {width:.3} {height:.3} re f",
            color.0, color.1, color.2
        );
        let mut ap_dict = Dictionary::new();
        ap_dict.insert("Type", Object::Name("XObject".to_string()));
        ap_dict.insert("Subtype", Object::Name("Form".to_string()));
        ap_dict.insert(
            "BBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Real(width),
                Object::Real(height),
            ]),
        );
        let ap_stream = Object::Stream(Stream {
            dict: ap_dict,
            raw_data: ap_content.into_bytes(),
        });
        self.pending.insert(ap_num, ap_stream);

        let annot_num = self.alloc_num();
        let mut annot_dict = Dictionary::new();
        annot_dict.insert("Type", Object::Name("Annot".to_string()));
        annot_dict.insert("Subtype", Object::Name("Highlight".to_string()));
        annot_dict.insert(
            "Rect",
            Object::Array(vec![
                Object::Real(x0),
                Object::Real(y0),
                Object::Real(x1),
                Object::Real(y1),
            ]),
        );
        annot_dict.insert(
            "QuadPoints",
            Object::Array(quad.into_iter().map(Object::Real).collect()),
        );
        annot_dict.insert(
            "C",
            Object::Array(vec![
                Object::Real(color.0 as f64),
                Object::Real(color.1 as f64),
                Object::Real(color.2 as f64),
            ]),
        );
        let mut ap_ref_dict = Dictionary::new();
        ap_ref_dict.insert("N", Object::Reference(ObjRef::new(ap_num, 0)));
        annot_dict.insert("AP", Object::Dictionary(ap_ref_dict));
        self.pending
            .insert(annot_num, Object::Dictionary(annot_dict));

        self.append_to_annots(page_ref, ObjRef::new(annot_num, 0))?;
        Ok(())
    }

    /// Ajoute la référence `new_annot` à `/Annots` de la page/objet
    /// `page_ref` — que `/Annots` soit absent, un tableau inline dans le
    /// dictionnaire de page, ou une référence vers un objet tableau séparé
    /// (dans ce dernier cas, c'est cet objet séparé qui est mis à jour, pas
    /// le dictionnaire de page, pour ne rien casser d'autre qui le
    /// référencerait).
    fn append_to_annots(&mut self, page_ref: ObjRef, new_annot: ObjRef) -> Result<(), String> {
        let page_obj = self.current(page_ref.num)?;
        let mut page_dict = page_obj
            .as_dict()
            .cloned()
            .ok_or_else(|| "page object is not a dictionary".to_string())?;

        match page_dict.get("Annots").cloned() {
            Some(Object::Reference(annots_ref)) => {
                let before = self.current(annots_ref.num)?;
                let mut items = before.as_array().map(|a| a.to_vec()).unwrap_or_default();
                items.push(Object::Reference(new_annot));
                let after = Object::Array(items);
                self.commit(EditOp {
                    modified: vec![(annots_ref.num, before, after)],
                });
            }
            Some(Object::Array(items)) => {
                let before_page = Object::Dictionary(page_dict.clone());
                let mut items = items;
                items.push(Object::Reference(new_annot));
                page_dict.insert("Annots", Object::Array(items));
                let after_page = Object::Dictionary(page_dict);
                self.commit(EditOp {
                    modified: vec![(page_ref.num, before_page, after_page)],
                });
            }
            _ => {
                let before_page = Object::Dictionary(page_dict.clone());
                page_dict.insert("Annots", Object::Array(vec![Object::Reference(new_annot)]));
                let after_page = Object::Dictionary(page_dict);
                self.commit(EditOp {
                    modified: vec![(page_ref.num, before_page, after_page)],
                });
            }
        }
        Ok(())
    }

    /// Alloue un objet `/Font` `/Helvetica` minimal (pas de `/FontFile` :
    /// résolu par la substitution système de `pdf-core::font` au rendu,
    /// comme n'importe quel PDF référençant une police standard non
    /// intégrée) et le nom de ressource `/Helv` qui lui correspond dans les
    /// flux d'apparence générés par cette session.
    fn alloc_helvetica_font(&mut self) -> ObjRef {
        let num = self.alloc_num();
        let mut dict = Dictionary::new();
        dict.insert("Type", Object::Name("Font".to_string()));
        dict.insert("Subtype", Object::Name("Type1".to_string()));
        dict.insert("BaseFont", Object::Name("Helvetica".to_string()));
        dict.insert("Encoding", Object::Name("WinAnsiEncoding".to_string()));
        let r = ObjRef::new(num, 0);
        self.pending.insert(num, Object::Dictionary(dict));
        r
    }

    /// Trouve le champ `/AcroForm/Fields` de nom `/T` == `field_name`
    /// (correspondance directe, un seul niveau — voir la doc de module) et
    /// renvoie sa référence d'objet.
    fn find_form_field(&self, field_name: &str) -> Result<ObjRef, String> {
        let root = self.doc.root().map_err(|e| e.to_string())?;
        let acroform_obj = root
            .get("AcroForm")
            .ok_or_else(|| "document has no /AcroForm".to_string())?;
        let acroform = self.doc.get(acroform_obj).map_err(|e| e.to_string())?;
        let acroform_dict = acroform
            .as_dict()
            .ok_or_else(|| "/AcroForm is not a dictionary".to_string())?;
        let fields_obj = acroform_dict
            .get("Fields")
            .ok_or_else(|| "/AcroForm has no /Fields".to_string())?;
        let fields = self.doc.get(fields_obj).map_err(|e| e.to_string())?;
        let field_refs = fields
            .as_array()
            .ok_or_else(|| "/Fields is not an array".to_string())?;

        for field_obj in field_refs {
            let Object::Reference(r) = field_obj else {
                continue;
            };
            let resolved = self.doc.resolve(*r).map_err(|e| e.to_string())?;
            let Some(dict) = resolved.as_dict() else {
                continue;
            };
            if dict.get("T").and_then(|o| o.as_text_string()) == Some(field_name.to_string()) {
                return Ok(*r);
            }
        }
        Err(format!("no form field named '{field_name}'"))
    }

    /// Fixe la valeur (`/V`) du champ de formulaire texte `field_name` et
    /// régénère son apparence (`/AP /N`) pour que la nouvelle valeur soit
    /// effectivement visible au rendu (`pdf-core::interp` ne synthétise pas
    /// d'apparence à partir de `/V`/`/DA` par lui-même, contrairement à
    /// certains lecteurs avec `/NeedAppearances` — voir la doc de module).
    pub fn set_form_field_value(&mut self, field_name: &str, value: &str) -> Result<(), String> {
        let field_ref = self.find_form_field(field_name)?;
        let before = self.current(field_ref.num)?;
        let mut field_dict = before
            .as_dict()
            .cloned()
            .ok_or_else(|| "form field object is not a dictionary".to_string())?;

        let rect = field_dict
            .get("Rect")
            .and_then(|o| o.as_array())
            .filter(|r| r.len() >= 4)
            .map(|r| [num(&r[0]), num(&r[1]), num(&r[2]), num(&r[3])])
            .ok_or_else(|| "form field has no usable /Rect".to_string())?;
        let width = (rect[2] - rect[0]).abs().max(1.0);
        let height = (rect[3] - rect[1]).abs().max(1.0);
        let font_size = (height * 0.7).clamp(6.0, 18.0);

        let font_ref = self.alloc_helvetica_font();
        let ap_num = self.alloc_num();
        let baseline_y = (height - font_size).max(1.0) / 2.0 + font_size * 0.2;
        let ap_content = format!(
            "/Tx BMC\nq\nBT\n/Helv {font_size:.2} Tf\n0 0 0 rg\n2 {baseline_y:.2} Td\n({}) Tj\nET\nQ\nEMC",
            escape_pdf_literal(value)
        );
        let mut font_res = Dictionary::new();
        font_res.insert("Helv", Object::Reference(font_ref));
        let mut resources = Dictionary::new();
        resources.insert("Font", Object::Dictionary(font_res));

        let mut ap_dict = Dictionary::new();
        ap_dict.insert("Type", Object::Name("XObject".to_string()));
        ap_dict.insert("Subtype", Object::Name("Form".to_string()));
        ap_dict.insert(
            "BBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Real(width),
                Object::Real(height),
            ]),
        );
        ap_dict.insert("Resources", Object::Dictionary(resources));
        let ap_stream = Object::Stream(Stream {
            dict: ap_dict,
            raw_data: ap_content.into_bytes(),
        });
        self.pending.insert(ap_num, ap_stream);

        field_dict.insert("V", Object::String(value.as_bytes().to_vec()));
        let mut ap_ref_dict = Dictionary::new();
        ap_ref_dict.insert("N", Object::Reference(ObjRef::new(ap_num, 0)));
        field_dict.insert("AP", Object::Dictionary(ap_ref_dict));

        let after = Object::Dictionary(field_dict);
        self.commit(EditOp {
            modified: vec![(field_ref.num, before, after)],
        });
        Ok(())
    }

    /// Sauvegarde incrémentale (Sprint 13-14, `Document::save_incremental`)
    /// vers `path` : ajoute tous les objets en attente au fichier original,
    /// sans jamais le modifier en place.
    pub fn save_as(&self, path: impl AsRef<Path>) -> Result<(), String> {
        let objects: Vec<(ObjRef, Object)> = self
            .pending
            .iter()
            .map(|(&num, obj)| (ObjRef::new(num, 0), obj.clone()))
            .collect();
        let bytes = self
            .doc
            .save_incremental(&objects)
            .map_err(|e| e.to_string())?;
        std::fs::write(path, bytes).map_err(|e| e.to_string())
    }
}

fn num(o: &Object) -> f64 {
    match o {
        Object::Integer(n) => *n as f64,
        Object::Real(f) => *f,
        _ => 0.0,
    }
}

/// Échappe `\`, `(` et `)` pour la syntaxe de chaîne littérale d'un flux de
/// contenu (`(...)` Tj) — ISO 32000-1 §7.3.4.2. Ne gère que l'ASCII/Latin-1
/// (suffisant pour `WinAnsiEncoding`, la police utilisée par les apparences
/// générées ici) ; les caractères hors de cette plage sont remplacés par
/// `?` plutôt que de produire un flux invalide.
fn escape_pdf_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '(' => out.push_str("\\("),
            ')' => out.push_str("\\)"),
            c if (c as u32) < 256 => out.push(c),
            _ => out.push('?'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdf_core::interp::Interpreter;

    fn write_fixture(bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "pdf_edit_test_{}_{:p}.pdf",
            std::process::id(),
            bytes
        ));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// Bout en bout : ajouter un surlignage, sauvegarder incrémentalement,
    /// rouvrir le fichier obtenu et vérifier qu'il se rend réellement (pas
    /// seulement que la structure `/Annots` existe).
    #[test]
    fn add_highlight_persists_and_renders_after_reopen() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);

        let mut session = EditSession::open(&path).unwrap();
        session
            .add_highlight_annotation(0, [100.0, 600.0, 300.0, 630.0], (1.0, 1.0, 0.0), vec![])
            .unwrap();

        let out_path = write_fixture(b"placeholder-for-unique-name");
        session.save_as(&out_path).unwrap();

        let reopened_bytes = std::fs::read(&out_path).unwrap();
        let doc = pdf_core::Document::open(reopened_bytes).unwrap();
        let page = doc.page(0).unwrap();
        let content = doc.page_content(&page).unwrap();
        let display = Interpreter::run_page_with_annotations(&doc, &page, &content).unwrap();

        let has_yellow_fill = display.items.iter().any(|item| {
            matches!(
                item,
                pdf_core::display::DisplayItem::Path {
                    fill_color: pdf_core::display::Color::Rgb(r, g, b),
                    ..
                } if (*r - 1.0).abs() < 0.01 && (*g - 1.0).abs() < 0.01 && *b < 0.01
            )
        });
        assert!(
            has_yellow_fill,
            "expected the highlight's yellow fill to survive save+reopen, got: {:?}",
            display.items
        );

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&out_path).ok();
    }

    /// Undo doit retirer l'annotation de `/Annots` (elle redevient
    /// invisible), redo doit la faire réapparaître — sans jamais retoucher
    /// le fichier sur disque (`EditSession` ne persiste qu'à `save_as`).
    #[test]
    fn undo_and_redo_toggle_annotation_visibility() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);
        let mut session = EditSession::open(&path).unwrap();

        assert!(!session.can_undo());
        session
            .add_highlight_annotation(0, [100.0, 600.0, 300.0, 630.0], (1.0, 1.0, 0.0), vec![])
            .unwrap();
        assert!(session.can_undo());
        assert!(!session.can_redo());

        let page_num = session.doc.page(0).unwrap().object_ref.unwrap().num;

        let annots_after_add = session
            .current(page_num)
            .unwrap()
            .as_dict()
            .unwrap()
            .get("Annots")
            .cloned();
        assert!(matches!(annots_after_add, Some(Object::Array(items)) if items.len() == 1));

        assert!(session.undo());
        let annots_after_undo = session
            .current(page_num)
            .unwrap()
            .as_dict()
            .unwrap()
            .get("Annots")
            .cloned();
        assert!(
            annots_after_undo.is_none()
                || matches!(annots_after_undo, Some(Object::Array(items)) if items.is_empty())
        );
        assert!(session.can_redo());

        assert!(session.redo());
        let annots_after_redo = session
            .current(page_num)
            .unwrap()
            .as_dict()
            .unwrap()
            .get("Annots")
            .cloned();
        assert!(matches!(annots_after_redo, Some(Object::Array(items)) if items.len() == 1));

        std::fs::remove_file(&path).ok();
    }

    /// Bout en bout sur un vrai formulaire : remplir un champ, sauvegarder,
    /// rouvrir, vérifier que `/V` a la nouvelle valeur **et** qu'un vrai
    /// glyphe est produit par l'apparence régénérée (pas juste `/V` mis à
    /// jour sans effet visible).
    #[test]
    fn set_form_field_value_persists_and_renders_after_reopen() {
        let bytes = include_bytes!("../../pdf-core/tests/fixtures/acroform_textfield.pdf").to_vec();
        let path = write_fixture(&bytes);

        let mut session = EditSession::open(&path).unwrap();
        session
            .set_form_field_value("name_field", "Ada Lovelace")
            .unwrap();

        let out_path = write_fixture(b"placeholder-for-unique-name-2");
        session.save_as(&out_path).unwrap();

        let reopened_bytes = std::fs::read(&out_path).unwrap();
        let doc = pdf_core::Document::open(reopened_bytes).unwrap();
        let root = doc.root().unwrap();
        let acroform = doc.get(root.get("AcroForm").unwrap()).unwrap();
        let fields = doc
            .get(acroform.as_dict().unwrap().get("Fields").unwrap())
            .unwrap();
        let field_ref = fields.as_array().unwrap()[0].as_reference().unwrap();
        let field = doc.resolve(field_ref).unwrap();
        assert_eq!(
            field
                .as_dict()
                .unwrap()
                .get("V")
                .and_then(|o| o.as_text_string()),
            Some("Ada Lovelace".to_string())
        );

        let page = doc.page(0).unwrap();
        let content = doc.page_content(&page).unwrap();
        let display = Interpreter::run_page_with_annotations(&doc, &page, &content).unwrap();
        let glyph_count = display
            .items
            .iter()
            .filter(|i| matches!(i, pdf_core::display::DisplayItem::Glyph { .. }))
            .count();
        assert!(
            glyph_count >= "Ada Lovelace".len(),
            "expected at least one glyph per character of the filled value, got {glyph_count}"
        );

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&out_path).ok();
    }
}
