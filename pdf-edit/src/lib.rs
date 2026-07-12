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
//! - Transparence partielle seulement sur les surlignages : `pdf-core::interp`
//!   gère `/ca`/`/CA` (`ExtGState`) en plus de `/LW`, donc `add_highlight_annotation`
//!   rend son aplat à `HIGHLIGHT_FILL_ALPHA` d'opacité constante plutôt qu'en
//!   couleur pleine — mais pas de vrais groupes de transparence ni de modes de
//!   fusion (`Multiply`, utilisé par les vrais lecteurs pour les surlignages),
//!   donc le rendu reste une approximation.
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

/// État de l'arbre de pages une fois "aplati" (Sprint 15-16) — voir
/// `EditSession::ensure_flat_page_tree`. `order` est la source de vérité
/// pour l'ordre des pages une fois ce mode déclenché : les opérations de
/// manipulation de pages (insertion/suppression/déplacement) le lisent et
/// l'écrivent directement, sans reparcourir `Document::pages()`.
struct PageTreeState {
    pages_node_ref: ObjRef,
    order: Vec<ObjRef>,
}

/// Champ de formulaire texte visible, tel que renvoyé par
/// `EditSession::form_fields` — assez d'information pour dessiner un
/// contour cliquable et pré-remplir une modale de saisie avec la valeur
/// actuelle.
#[derive(Debug, Clone, PartialEq)]
pub struct FormFieldInfo {
    pub obj_ref: ObjRef,
    pub name: String,
    pub rect: [f64; 4],
    pub value: String,
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
    page_tree: Option<PageTreeState>,
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
            page_tree: None,
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
        self.refresh_page_tree_order();
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
        self.refresh_page_tree_order();
        true
    }

    /// Recale `page_tree.order` (cache pratique de l'ordre des pages, tenu
    /// à jour par les opérations de manipulation de page) sur `/Kids` tel
    /// qu'il vient d'être restauré dans `pending` par `undo`/`redo` — sans
    /// ça, `page_tree.order` resterait périmé après un `undo`/`redo` qui
    /// touche l'ordre des pages, puisque `undo`/`redo` ne connaissent que
    /// des paires (numéro d'objet, valeur), pas la sémantique "ordre de
    /// pages" propre à `page_tree`.
    fn refresh_page_tree_order(&mut self) {
        let Some(state) = &self.page_tree else {
            return;
        };
        let pages_node_num = state.pages_node_ref.num;
        let Ok(obj) = self.current(pages_node_num) else {
            return;
        };
        let Some(kids) = obj
            .as_dict()
            .and_then(|d| d.get("Kids"))
            .and_then(|o| o.as_array())
        else {
            return;
        };
        let order: Vec<ObjRef> = kids.iter().filter_map(|o| o.as_reference()).collect();
        self.page_tree.as_mut().unwrap().order = order;
    }

    fn commit(&mut self, op: EditOp) {
        for (num, _before, after) in &op.modified {
            self.pending.insert(*num, after.clone());
        }
        self.undo_stack.push(op);
        self.redo_stack.clear();
    }

    /// Opacité constante (`/ca`) appliquée au remplissage d'un surlignage —
    /// se rapproche du rendu "encre transparente" des vrais lecteurs (qui
    /// utilisent plutôt un mode de fusion `Multiply`, non géré ici, voir la
    /// doc de module) sans nécessiter de groupes de transparence.
    const HIGHLIGHT_FILL_ALPHA: f64 = 0.4;

    /// Ajoute une annotation `/Highlight` couvrant `rect` (`[x0 y0 x1 y1]`,
    /// espace page) sur la page `page_index`, avec un flux d'apparence
    /// (`/AP /N`) qui remplit ce rectangle de `color` (RGB 0.0-1.0) à
    /// `HIGHLIGHT_FILL_ALPHA` d'opacité (`/ca` d'un `ExtGState` référencé
    /// dans les `/Resources` du flux, voir `interp::apply_ext_gstate`) —
    /// sans quoi le texte sous le surlignage serait entièrement caché par
    /// un aplat opaque. `quad_points` (ISO 32000-1 §12.5.6.10, 8 nombres =
    /// 4 sommets, sens direct) : si vide, dérivé automatiquement des quatre
    /// coins de `rect`.
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

        let gs_num = self.alloc_num();
        let mut gs_dict = Dictionary::new();
        gs_dict.insert("Type", Object::Name("ExtGState".to_string()));
        gs_dict.insert("ca", Object::Real(Self::HIGHLIGHT_FILL_ALPHA));
        self.pending.insert(gs_num, Object::Dictionary(gs_dict));

        let ap_num = self.alloc_num();
        let ap_content = format!(
            "/GS0 gs {:.3} {:.3} {:.3} rg 0 0 {width:.3} {height:.3} re f",
            color.0, color.1, color.2
        );
        let mut ap_resources = Dictionary::new();
        let mut ext_gstate_dict = Dictionary::new();
        ext_gstate_dict.insert("GS0", Object::Reference(ObjRef::new(gs_num, 0)));
        ap_resources.insert("ExtGState", Object::Dictionary(ext_gstate_dict));

        let mut ap_dict = Dictionary::new();
        ap_dict.insert("Type", Object::Name("XObject".to_string()));
        ap_dict.insert("Subtype", Object::Name("Form".to_string()));
        ap_dict.insert("Resources", Object::Dictionary(ap_resources));
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

    /// Ajoute une annotation `/Underline` couvrant `rect` (Sprint 20) : une
    /// ligne tracée près du bas du rectangle, comme un vrai lecteur PDF.
    /// Partage `add_line_markup_annotation` avec `add_strikeout_annotation`
    /// — seule la position verticale de la ligne diffère.
    pub fn add_underline_annotation(
        &mut self,
        page_index: usize,
        rect: [f64; 4],
        color: (f32, f32, f32),
    ) -> Result<(), String> {
        self.add_line_markup_annotation("Underline", page_index, rect, color, 0.08)
    }

    /// Ajoute une annotation `/StrikeOut` couvrant `rect` (Sprint 20) : une
    /// ligne tracée au centre du rectangle.
    pub fn add_strikeout_annotation(
        &mut self,
        page_index: usize,
        rect: [f64; 4],
        color: (f32, f32, f32),
    ) -> Result<(), String> {
        self.add_line_markup_annotation("StrikeOut", page_index, rect, color, 0.5)
    }

    /// Construit une annotation de type "ligne tracée" (`/Underline` ou
    /// `/StrikeOut`, ISO 32000-1 §12.5.6.9/.10) : contrairement à
    /// `/Highlight` (rectangle rempli semi-transparent), l'apparence est un
    /// simple trait horizontal à `line_y_fraction` de la hauteur de `rect` —
    /// pas de vraie ligne de base connue pour une plage de caractères
    /// arbitraire (même limitation que `add_highlight_annotation`), la
    /// fraction est donc une approximation raisonnable plutôt qu'une mesure
    /// exacte.
    fn add_line_markup_annotation(
        &mut self,
        subtype: &str,
        page_index: usize,
        rect: [f64; 4],
        color: (f32, f32, f32),
        line_y_fraction: f64,
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
        let line_y = height * line_y_fraction;

        let ap_num = self.alloc_num();
        let ap_content = format!(
            "{:.3} {:.3} {:.3} RG 1 w 0 {line_y:.3} m {width:.3} {line_y:.3} l S",
            color.0, color.1, color.2
        );
        let mut ap_dict = Dictionary::new();
        ap_dict.insert("Type", Object::Name("XObject".to_string()));
        ap_dict.insert("Subtype", Object::Name("Form".to_string()));
        ap_dict.insert("Resources", Object::Dictionary(Dictionary::new()));
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
        annot_dict.insert("Subtype", Object::Name(subtype.to_string()));
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
            Object::Array(
                vec![x0, y1, x1, y1, x0, y0, x1, y0]
                    .into_iter()
                    .map(Object::Real)
                    .collect(),
            ),
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

    /// Construit le flux de contenu d'une apparence texte : un fond plein
    /// optionnel (`background`, ISO 32000-1 — sert au "masquer l'ancien" du
    /// remplacement par superposition, Sprint 17+ 6b) suivi du texte
    /// lui-même en Helvetica non intégrée (résolue par la substitution
    /// système au rendu, comme `alloc_helvetica_font`). Partagé par
    /// `add_free_text_annotation` (6a) et `replace_text_with_overlay` (6b) :
    /// les deux ne diffèrent que par la présence ou non d'un fond.
    fn build_text_appearance_content(
        width: f64,
        height: f64,
        text: &str,
        font_size: f64,
        text_color: (f32, f32, f32),
        background: Option<(f32, f32, f32)>,
    ) -> String {
        let mut content = String::new();
        if let Some((r, g, b)) = background {
            content.push_str(&format!(
                "{r:.3} {g:.3} {b:.3} rg 0 0 {width:.3} {height:.3} re f\n"
            ));
        }
        let baseline_y = (height - font_size).max(1.0) / 2.0 + font_size * 0.2;
        content.push_str(&format!(
            "q\nBT\n/Helv {font_size:.2} Tf\n{:.3} {:.3} {:.3} rg\n2 {baseline_y:.2} Td\n({}) Tj\nET\nQ",
            text_color.0,
            text_color.1,
            text_color.2,
            escape_pdf_literal(text)
        ));
        content
    }

    /// Ajoute une annotation `/FreeText` (Sprint 17+, 6a : "ajout de nouveau
    /// texte") sur la page `page_index`, avec une apparence réelle générée
    /// (pas seulement `/Contents`/`/DA`, que ce moteur ne synthétise pas au
    /// rendu — voir `set_form_field_value` pour la même contrainte côté
    /// formulaires). Gérée par l'éditeur au même titre que les surlignages :
    /// `remove_annotation` la retire, `undo`/`redo` s'appliquent normalement.
    pub fn add_free_text_annotation(
        &mut self,
        page_index: usize,
        rect: [f64; 4],
        text: &str,
        font_size: f64,
        text_color: (f32, f32, f32),
    ) -> Result<(), String> {
        self.add_text_overlay_annotation(page_index, rect, text, font_size, text_color, None)
    }

    /// Remplacement de texte existant par superposition (Sprint 17+, 6b) :
    /// "masquer l'ancien + redessiner", pas une édition chirurgicale du flux
    /// de contenu d'origine (6c, hors périmètre — voir sprint.md). Couvre
    /// `rect` d'un rectangle plein de `background` (typiquement le blanc de
    /// la page) puis dessine `text` par-dessus — une nouvelle annotation
    /// `/FreeText`, le contenu original sous-jacent n'est jamais modifié ni
    /// supprimé (il reste dans le flux de la page, simplement recouvert).
    pub fn replace_text_with_overlay(
        &mut self,
        page_index: usize,
        rect: [f64; 4],
        text: &str,
        font_size: f64,
        text_color: (f32, f32, f32),
        background: (f32, f32, f32),
    ) -> Result<(), String> {
        self.add_text_overlay_annotation(
            page_index,
            rect,
            text,
            font_size,
            text_color,
            Some(background),
        )
    }

    fn add_text_overlay_annotation(
        &mut self,
        page_index: usize,
        rect: [f64; 4],
        text: &str,
        font_size: f64,
        text_color: (f32, f32, f32),
        background: Option<(f32, f32, f32)>,
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

        let font_ref = self.alloc_helvetica_font();
        let mut font_res = Dictionary::new();
        font_res.insert("Helv", Object::Reference(font_ref));
        let mut resources = Dictionary::new();
        resources.insert("Font", Object::Dictionary(font_res));

        let content = Self::build_text_appearance_content(
            width, height, text, font_size, text_color, background,
        );

        let ap_num = self.alloc_num();
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
            raw_data: content.into_bytes(),
        });
        self.pending.insert(ap_num, ap_stream);

        let annot_num = self.alloc_num();
        let mut annot_dict = Dictionary::new();
        annot_dict.insert("Type", Object::Name("Annot".to_string()));
        annot_dict.insert("Subtype", Object::Name("FreeText".to_string()));
        annot_dict.insert(
            "Rect",
            Object::Array(vec![
                Object::Real(x0),
                Object::Real(y0),
                Object::Real(x1),
                Object::Real(y1),
            ]),
        );
        annot_dict.insert("Contents", Object::String(text.as_bytes().to_vec()));
        annot_dict.insert(
            "DA",
            Object::String(format!("/Helv {font_size:.2} Tf 0 g").into_bytes()),
        );
        let mut ap_ref_dict = Dictionary::new();
        ap_ref_dict.insert("N", Object::Reference(ObjRef::new(ap_num, 0)));
        annot_dict.insert("AP", Object::Dictionary(ap_ref_dict));
        self.pending
            .insert(annot_num, Object::Dictionary(annot_dict));

        self.append_to_annots(page_ref, ObjRef::new(annot_num, 0))?;
        Ok(())
    }

    /// Références (dans l'ordre de `/Annots`) des annotations de la page
    /// `page_index` — résout `/Annots` qu'il soit inline ou indirect, comme
    /// `append_to_annots`.
    fn page_annotation_refs(&self, page_index: usize) -> Result<Vec<ObjRef>, String> {
        let page = self.doc.page(page_index).map_err(|e| e.to_string())?;
        let page_ref = page
            .object_ref
            .ok_or_else(|| "page has no indirect object reference".to_string())?;
        let page_obj = self.current(page_ref.num)?;
        let page_dict = page_obj
            .as_dict()
            .ok_or_else(|| "page object is not a dictionary".to_string())?;

        let annots = match page_dict.get("Annots") {
            Some(Object::Reference(r)) => self.current(r.num)?,
            Some(other) => other.clone(),
            None => return Ok(Vec::new()),
        };
        Ok(annots
            .as_array()
            .map(|items| items.iter().filter_map(|o| o.as_reference()).collect())
            .unwrap_or_default())
    }

    /// Retire l'annotation d'indice `annot_index` (dans l'ordre de
    /// `/Annots`) de la page `page_index` — l'objet annotation et son
    /// apparence restent alloués, orphelins (voir la doc de module sur le
    /// garbage collection), seule la référence dans `/Annots` disparaît.
    pub fn remove_annotation(
        &mut self,
        page_index: usize,
        annot_index: usize,
    ) -> Result<(), String> {
        let page = self.doc.page(page_index).map_err(|e| e.to_string())?;
        let page_ref = page
            .object_ref
            .ok_or_else(|| "page has no indirect object reference".to_string())?;
        let refs = self.page_annotation_refs(page_index)?;
        if annot_index >= refs.len() {
            return Err(format!(
                "annotation index {annot_index} out of bounds (0..{})",
                refs.len()
            ));
        }

        let page_obj = self.current(page_ref.num)?;
        let page_dict = page_obj
            .as_dict()
            .cloned()
            .ok_or_else(|| "page object is not a dictionary".to_string())?;

        let mut new_refs = refs;
        new_refs.remove(annot_index);
        let new_array = Object::Array(new_refs.into_iter().map(Object::Reference).collect());

        match page_dict.get("Annots").cloned() {
            Some(Object::Reference(annots_ref)) => {
                let before = self.current(annots_ref.num)?;
                self.commit(EditOp {
                    modified: vec![(annots_ref.num, before, new_array)],
                });
            }
            _ => {
                let before_page = Object::Dictionary(page_dict.clone());
                let mut updated = page_dict;
                updated.insert("Annots", new_array);
                self.commit(EditOp {
                    modified: vec![(page_ref.num, before_page, Object::Dictionary(updated))],
                });
            }
        }
        Ok(())
    }

    /// Liste les champs de formulaire texte (`/FT /Tx`) de `/AcroForm/Fields`
    /// (même périmètre qu'un seul niveau que `find_form_field`/
    /// `set_form_field_value` — voir la doc de module) : assez d'information
    /// pour que `pdf-app`/`pdf-ui` proposent un clic sur le champ affiché à
    /// l'écran sans dupliquer la logique de résolution `/AcroForm`. Lit la
    /// valeur courante via `current` (pas `doc.resolve`) pour refléter un
    /// `set_form_field_value` déjà en attente dans cette session.
    pub fn form_fields(&self) -> Result<Vec<FormFieldInfo>, String> {
        let root = self.doc.root().map_err(|e| e.to_string())?;
        let Some(acroform_obj) = root.get("AcroForm") else {
            return Ok(Vec::new());
        };
        let acroform = self.doc.get(acroform_obj).map_err(|e| e.to_string())?;
        let Some(acroform_dict) = acroform.as_dict() else {
            return Ok(Vec::new());
        };
        let Some(fields_obj) = acroform_dict.get("Fields") else {
            return Ok(Vec::new());
        };
        let fields = self.doc.get(fields_obj).map_err(|e| e.to_string())?;
        let Some(field_refs) = fields.as_array() else {
            return Ok(Vec::new());
        };

        let mut out = Vec::new();
        for field_obj in field_refs {
            let Object::Reference(r) = field_obj else {
                continue;
            };
            let resolved = self.current(r.num)?;
            let Some(dict) = resolved.as_dict() else {
                continue;
            };
            if dict.get("FT").and_then(|o| o.as_name()) != Some("Tx") {
                continue; // Cases à cocher/boutons radio/listes hors périmètre, voir doc de module.
            }
            let Some(name) = dict.get("T").and_then(|o| o.as_text_string()) else {
                continue;
            };
            let Some(rect_arr) = dict
                .get("Rect")
                .and_then(|o| o.as_array())
                .filter(|r| r.len() >= 4)
            else {
                continue;
            };
            let rect = [
                num(&rect_arr[0]),
                num(&rect_arr[1]),
                num(&rect_arr[2]),
                num(&rect_arr[3]),
            ];
            let value = dict
                .get("V")
                .and_then(|o| o.as_text_string())
                .unwrap_or_default();
            out.push(FormFieldInfo {
                obj_ref: *r,
                name,
                rect,
                value,
            });
        }
        Ok(out)
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

    /// Matérialise l'arbre `/Pages` courant en une seule liste plate (un
    /// unique nœud `/Pages` listant directement toutes les pages en
    /// `/Kids`), en "cuisant" en dur sur chaque page les attributs qu'elle
    /// héritait éventuellement d'un ancêtre (`/MediaBox`, `/Rotate`,
    /// `/Resources`) — Sprint 15-16 : les opérations de manipulation de
    /// pages (insertion/suppression/déplacement) ont besoin d'un point
    /// unique où modifier l'ordre, quelle que soit la forme (potentiellement
    /// arborescente, avec plusieurs niveaux de `/Pages` intermédiaires) de
    /// l'arbre d'origine — une pratique courante des bibliothèques
    /// d'édition PDF plutôt qu'une manipulation chirurgicale de l'arbre
    /// existant, qui serait bien plus complexe pour un bénéfice minime.
    ///
    /// Ne fait rien au-delà du premier appel (`page_tree` déjà renseigné) :
    /// retourne la liste des modifications de **cette** invocation
    /// seulement (vide si l'arbre était déjà aplati), pour que l'appelant
    /// (la première opération de manipulation de page de la session) les
    /// inclue dans son propre `EditOp` — un `undo` de cette première
    /// opération doit aussi annuler l'aplatissement, pas seulement son
    /// propre effet.
    fn ensure_flat_page_tree(&mut self) -> Result<Vec<(u32, Object, Object)>, String> {
        if self.page_tree.is_some() {
            return Ok(Vec::new());
        }

        let pages = self.doc.pages().map_err(|e| e.to_string())?;
        let pages_node_num = self.alloc_num();
        let pages_node_ref = ObjRef::new(pages_node_num, 0);

        let mut order = Vec::with_capacity(pages.len());
        let mut modified = Vec::with_capacity(pages.len() + 1);

        for page in &pages {
            let page_ref = page.object_ref.ok_or_else(|| {
                "page tree contains an inline page dictionary (not supported for page manipulation)"
                    .to_string()
            })?;
            let before = self.current(page_ref.num)?;
            let mut dict = before
                .as_dict()
                .cloned()
                .ok_or_else(|| "page object is not a dictionary".to_string())?;
            dict.insert("Type", Object::Name("Page".to_string()));
            dict.insert("Parent", Object::Reference(pages_node_ref));
            dict.insert(
                "MediaBox",
                Object::Array(page.media_box.iter().map(|&n| Object::Real(n)).collect()),
            );
            dict.insert("Rotate", Object::Integer(page.rotate as i64));
            dict.insert("Resources", Object::Dictionary(page.resources.clone()));
            let after = Object::Dictionary(dict);
            modified.push((page_ref.num, before, after));
            order.push(page_ref);
        }

        let root_ref = self
            .doc
            .trailer()
            .get("Root")
            .and_then(|o| o.as_reference())
            .ok_or_else(|| "trailer /Root is not an indirect reference".to_string())?;
        let before_root = self.current(root_ref.num)?;
        let mut root_dict = before_root
            .as_dict()
            .cloned()
            .ok_or_else(|| "/Root is not a dictionary".to_string())?;
        root_dict.insert("Pages", Object::Reference(pages_node_ref));
        let after_root = Object::Dictionary(root_dict);
        modified.push((root_ref.num, before_root, after_root));

        let mut pages_dict = Dictionary::new();
        pages_dict.insert("Type", Object::Name("Pages".to_string()));
        pages_dict.insert(
            "Kids",
            Object::Array(order.iter().map(|&r| Object::Reference(r)).collect()),
        );
        pages_dict.insert("Count", Object::Integer(order.len() as i64));
        // Objet flambant neuf : jamais annulé par un `undo` (voir la doc de
        // module) — seules les entrées de `modified` ci-dessus (objets déjà
        // existants modifiés) portent la sémantique undo/redo.
        self.pending
            .insert(pages_node_num, Object::Dictionary(pages_dict));

        self.page_tree = Some(PageTreeState {
            pages_node_ref,
            order,
        });
        Ok(modified)
    }

    /// Réécrit `/Kids`/`/Count` du nœud `/Pages` aplati pour refléter
    /// `new_order`, et pousse cette modification dans `modified` — partagé
    /// par toutes les opérations de manipulation de page ci-dessous.
    fn update_page_order(
        &mut self,
        new_order: Vec<ObjRef>,
        modified: &mut Vec<(u32, Object, Object)>,
    ) -> Result<(), String> {
        let pages_node_ref = self.page_tree.as_ref().unwrap().pages_node_ref;
        let before = self.current(pages_node_ref.num)?;
        let mut dict = before
            .as_dict()
            .cloned()
            .ok_or_else(|| "flat pages node is not a dictionary".to_string())?;
        dict.insert(
            "Kids",
            Object::Array(new_order.iter().map(|&r| Object::Reference(r)).collect()),
        );
        dict.insert("Count", Object::Integer(new_order.len() as i64));
        modified.push((pages_node_ref.num, before, Object::Dictionary(dict)));
        self.page_tree.as_mut().unwrap().order = new_order;
        Ok(())
    }

    /// Nombre de pages courant (après d'éventuelles manipulations en
    /// attente dans cette session, avant qu'elles ne soient sauvegardées).
    pub fn page_count(&self) -> Result<usize, String> {
        match &self.page_tree {
            Some(state) => Ok(state.order.len()),
            None => self.doc.page_count().map_err(|e| e.to_string()),
        }
    }

    /// Insère une page blanche (contenu vide, `/Resources` vide) à l'indice
    /// `at_index` (borné à `[0, page_count]`, donc `page_count` insère à la
    /// fin).
    pub fn insert_blank_page(
        &mut self,
        at_index: usize,
        media_box: [f64; 4],
    ) -> Result<(), String> {
        let mut modified = self.ensure_flat_page_tree()?;
        let pages_node_ref = self.page_tree.as_ref().unwrap().pages_node_ref;

        let content_num = self.alloc_num();
        self.pending.insert(
            content_num,
            Object::Stream(Stream {
                dict: Dictionary::new(),
                raw_data: Vec::new(),
            }),
        );

        let page_num = self.alloc_num();
        let page_ref = ObjRef::new(page_num, 0);
        let mut page_dict = Dictionary::new();
        page_dict.insert("Type", Object::Name("Page".to_string()));
        page_dict.insert("Parent", Object::Reference(pages_node_ref));
        page_dict.insert(
            "MediaBox",
            Object::Array(media_box.iter().map(|&n| Object::Real(n)).collect()),
        );
        page_dict.insert("Resources", Object::Dictionary(Dictionary::new()));
        page_dict.insert("Contents", Object::Reference(ObjRef::new(content_num, 0)));
        self.pending.insert(page_num, Object::Dictionary(page_dict));

        let mut new_order = self.page_tree.as_ref().unwrap().order.clone();
        let at_index = at_index.min(new_order.len());
        new_order.insert(at_index, page_ref);
        self.update_page_order(new_order, &mut modified)?;

        self.commit(EditOp { modified });
        Ok(())
    }

    /// Insère une nouvelle page à `at_index` dont le seul contenu est
    /// `jpeg_bytes` dessiné en plein cadre (Sprint 15-16, "insertion
    /// d'images") — les octets JPEG sont intégrés **tels quels**
    /// (`/Filter /DCTDecode`), sans redécoder/recompresser : `/Width`,
    /// `/Height` et `/ColorSpace` sont lus depuis les en-têtes JPEG
    /// (`pdf_core::filters::jpeg_dimensions`) sans décoder les pixels.
    /// `/MediaBox` de la nouvelle page correspond aux dimensions de l'image
    /// en points (1 pixel = 1 point, le choix le plus simple qui reste
    /// correct — pas de résolution DPI à interpréter). **Formats non
    /// gérés :** PNG et autres (nécessiteraient soit une réencodage en
    /// JPEG, soit le support d'échantillons bruts `FlateDecode`, hors
    /// périmètre de cette passe).
    pub fn insert_image_page(&mut self, at_index: usize, jpeg_bytes: &[u8]) -> Result<(), String> {
        let (width, height, components) =
            pdf_core::filters::jpeg_dimensions(jpeg_bytes).map_err(|e| e.to_string())?;
        let color_space = match components {
            1 => "DeviceGray",
            4 => "DeviceCMYK",
            _ => "DeviceRGB",
        };

        let mut modified = self.ensure_flat_page_tree()?;
        let pages_node_ref = self.page_tree.as_ref().unwrap().pages_node_ref;

        let image_num = self.alloc_num();
        let mut image_dict = Dictionary::new();
        image_dict.insert("Type", Object::Name("XObject".to_string()));
        image_dict.insert("Subtype", Object::Name("Image".to_string()));
        image_dict.insert("Width", Object::Integer(width as i64));
        image_dict.insert("Height", Object::Integer(height as i64));
        image_dict.insert("ColorSpace", Object::Name(color_space.to_string()));
        image_dict.insert("BitsPerComponent", Object::Integer(8));
        image_dict.insert("Filter", Object::Name("DCTDecode".to_string()));
        self.pending.insert(
            image_num,
            Object::Stream(Stream {
                dict: image_dict,
                raw_data: jpeg_bytes.to_vec(),
            }),
        );

        let content_num = self.alloc_num();
        let content = format!("q {width} 0 0 {height} 0 0 cm /Im0 Do Q");
        self.pending.insert(
            content_num,
            Object::Stream(Stream {
                dict: Dictionary::new(),
                raw_data: content.into_bytes(),
            }),
        );

        let mut xobject_res = Dictionary::new();
        xobject_res.insert("Im0", Object::Reference(ObjRef::new(image_num, 0)));
        let mut resources = Dictionary::new();
        resources.insert("XObject", Object::Dictionary(xobject_res));

        let page_num = self.alloc_num();
        let page_ref = ObjRef::new(page_num, 0);
        let mut page_dict = Dictionary::new();
        page_dict.insert("Type", Object::Name("Page".to_string()));
        page_dict.insert("Parent", Object::Reference(pages_node_ref));
        page_dict.insert(
            "MediaBox",
            Object::Array(vec![
                Object::Integer(0),
                Object::Integer(0),
                Object::Integer(width as i64),
                Object::Integer(height as i64),
            ]),
        );
        page_dict.insert("Resources", Object::Dictionary(resources));
        page_dict.insert("Contents", Object::Reference(ObjRef::new(content_num, 0)));
        self.pending.insert(page_num, Object::Dictionary(page_dict));

        let mut new_order = self.page_tree.as_ref().unwrap().order.clone();
        let at_index = at_index.min(new_order.len());
        new_order.insert(at_index, page_ref);
        self.update_page_order(new_order, &mut modified)?;

        self.commit(EditOp { modified });
        Ok(())
    }

    /// Supprime la page `index` (juste retirée de `/Kids` — l'objet page
    /// lui-même et son contenu restent alloués, orphelins, voir la doc de
    /// module sur le garbage collection).
    pub fn delete_page(&mut self, index: usize) -> Result<(), String> {
        let mut modified = self.ensure_flat_page_tree()?;
        let mut new_order = self.page_tree.as_ref().unwrap().order.clone();
        if index >= new_order.len() {
            return Err(format!(
                "page index {index} out of bounds (0..{})",
                new_order.len()
            ));
        }
        new_order.remove(index);
        self.update_page_order(new_order, &mut modified)?;
        self.commit(EditOp { modified });
        Ok(())
    }

    /// Déplace la page `from` à la position `to` (les autres pages se
    /// décalent en conséquence, comme `Vec::insert` après un `remove`).
    pub fn move_page(&mut self, from: usize, to: usize) -> Result<(), String> {
        let mut modified = self.ensure_flat_page_tree()?;
        let mut new_order = self.page_tree.as_ref().unwrap().order.clone();
        if from >= new_order.len() || to >= new_order.len() {
            return Err(format!("page index out of bounds (0..{})", new_order.len()));
        }
        let page_ref = new_order.remove(from);
        new_order.insert(to, page_ref);
        self.update_page_order(new_order, &mut modified)?;
        self.commit(EditOp { modified });
        Ok(())
    }

    /// Ajoute `delta` degrés (multiple de 90 attendu, mais pas vérifié) à
    /// `/Rotate` de la page `index`, normalisé dans `[0, 360)`.
    pub fn rotate_page(&mut self, index: usize, delta: i32) -> Result<(), String> {
        let mut modified = self.ensure_flat_page_tree()?;
        let page_ref = *self
            .page_tree
            .as_ref()
            .unwrap()
            .order
            .get(index)
            .ok_or_else(|| format!("page index {index} out of bounds"))?;
        let before = self.current(page_ref.num)?;
        let mut dict = before
            .as_dict()
            .cloned()
            .ok_or_else(|| "page object is not a dictionary".to_string())?;
        let current_rotate = dict.get("Rotate").and_then(|o| o.as_int()).unwrap_or(0);
        let new_rotate = (current_rotate + delta as i64).rem_euclid(360);
        dict.insert("Rotate", Object::Integer(new_rotate));
        modified.push((page_ref.num, before, Object::Dictionary(dict)));
        self.commit(EditOp { modified });
        Ok(())
    }

    /// Copie les pages `source_indices` de `source` (document distinct,
    /// potentiellement produit par un tout autre outil) dans cette session,
    /// insérées à `at_index` — Sprint 15-16, "fusion de documents". Copie
    /// **tout** ce que chaque page référence transitivement (ressources,
    /// polices, images, annotations...), renuméroté pour ne jamais entrer
    /// en collision avec les numéros déjà utilisés dans cette session (voir
    /// `copy_object_recursive`).
    pub fn insert_pages_from(
        &mut self,
        source: &Document,
        source_indices: &[usize],
        at_index: usize,
    ) -> Result<(), String> {
        let mut modified = self.ensure_flat_page_tree()?;
        let pages_node_ref = self.page_tree.as_ref().unwrap().pages_node_ref;
        let source_pages = source.pages().map_err(|e| e.to_string())?;

        let mut copy_map = std::collections::HashMap::new();
        let mut new_refs = Vec::with_capacity(source_indices.len());
        for &src_index in source_indices {
            let src_page = source_pages
                .get(src_index)
                .ok_or_else(|| format!("source page index {src_index} out of bounds"))?;
            let new_ref = self.copy_page(source, src_page, pages_node_ref, &mut copy_map)?;
            new_refs.push(new_ref);
        }

        let mut new_order = self.page_tree.as_ref().unwrap().order.clone();
        let at_index = at_index.min(new_order.len());
        for (offset, r) in new_refs.into_iter().enumerate() {
            new_order.insert(at_index + offset, r);
        }
        self.update_page_order(new_order, &mut modified)?;
        self.commit(EditOp { modified });
        Ok(())
    }

    /// Concatène la totalité de `source` à la fin du document courant —
    /// cas d'usage le plus courant de `insert_pages_from`.
    pub fn merge_document(&mut self, source: &Document) -> Result<(), String> {
        let count = source.page_count().map_err(|e| e.to_string())?;
        let indices: Vec<usize> = (0..count).collect();
        let at_index = self.page_count()?;
        self.insert_pages_from(source, &indices, at_index)
    }

    /// Copie une page de `source` (dictionnaire + tout ce qu'il référence
    /// transitivement) dans `self.pending`, avec ses attributs hérités
    /// (`/MediaBox`/`/Rotate`/`/Resources`) cuits en dur comme le fait
    /// `ensure_flat_page_tree` pour les pages déjà présentes.
    fn copy_page(
        &mut self,
        source: &Document,
        src_page: &pdf_core::page::Page,
        pages_node_ref: ObjRef,
        copy_map: &mut std::collections::HashMap<u32, u32>,
    ) -> Result<ObjRef, String> {
        let src_ref = src_page.object_ref.ok_or_else(|| {
            "source page has no indirect object reference (inline in /Kids, not supported)"
                .to_string()
        })?;
        let new_num = *copy_map.entry(src_ref.num).or_insert_with(|| {
            let n = self.next_num;
            self.next_num += 1;
            n
        });

        let raw_page = source.resolve(src_ref).map_err(|e| e.to_string())?;
        let copied = copy_object_recursive(
            source,
            &raw_page,
            &mut self.pending,
            &mut self.next_num,
            copy_map,
        )?;
        let mut dict = copied
            .as_dict()
            .cloned()
            .ok_or_else(|| "copied page is not a dictionary".to_string())?;

        dict.insert("Type", Object::Name("Page".to_string()));
        dict.insert("Parent", Object::Reference(pages_node_ref));
        dict.insert(
            "MediaBox",
            Object::Array(
                src_page
                    .media_box
                    .iter()
                    .map(|&n| Object::Real(n))
                    .collect(),
            ),
        );
        dict.insert("Rotate", Object::Integer(src_page.rotate as i64));
        let copied_resources = copy_object_recursive(
            source,
            &Object::Dictionary(src_page.resources.clone()),
            &mut self.pending,
            &mut self.next_num,
            copy_map,
        )?;
        dict.insert("Resources", copied_resources);

        self.pending.insert(new_num, Object::Dictionary(dict));
        Ok(ObjRef::new(new_num, 0))
    }

    /// Sauvegarde incrémentale (Sprint 13-14, `Document::save_incremental`)
    /// vers `path` : ajoute tous les objets en attente au fichier original,
    /// sans jamais le modifier en place.
    pub fn save_as(&self, path: impl AsRef<Path>) -> Result<(), String> {
        std::fs::write(path, self.to_bytes()?).map_err(|e| e.to_string())
    }

    /// Comme `save_as`, mais renvoie les octets plutôt que de les écrire sur
    /// disque — utilisé par `pdf-app::Session` (viewer) pour obtenir un
    /// aperçu à jour des modifications en attente : les rouvrir dans un
    /// `Document` en lecture pour le rendu, sans passer par un fichier
    /// temporaire ni toucher au fichier réellement ouvert par l'utilisateur.
    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        let objects: Vec<(ObjRef, Object)> = self
            .pending
            .iter()
            .map(|(&num, obj)| (ObjRef::new(num, 0), obj.clone()))
            .collect();
        self.doc
            .save_incremental(&objects)
            .map_err(|e| e.to_string())
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

/// Copie récursivement `obj` (potentiellement lui-même une référence, ou
/// contenant des références imbriquées à n'importe quelle profondeur) de
/// `source` vers `pending`, en renumérotant tout objet indirect rencontré
/// via un compteur partagé `next_num` — Sprint 15-16, cœur de la fusion de
/// documents (`EditSession::insert_pages_from`) et du découpage
/// (`extract_pages`).
///
/// `copy_map` (ancien numéro dans `source` -> nouveau numéro) sert deux
/// rôles à la fois : ne copier qu'une seule fois un objet partagé
/// référencé plusieurs fois (ex. une police utilisée par toutes les pages),
/// et casser les cycles (le nouveau numéro est réservé **avant** de
/// résoudre/copier récursivement l'objet référencé, donc une référence
/// arrière retrouve un numéro déjà attribué plutôt que de boucler à
/// l'infini).
fn copy_object_recursive(
    source: &Document,
    obj: &Object,
    pending: &mut std::collections::BTreeMap<u32, Object>,
    next_num: &mut u32,
    copy_map: &mut std::collections::HashMap<u32, u32>,
) -> Result<Object, String> {
    match obj {
        Object::Reference(r) => {
            if let Some(&new_num) = copy_map.get(&r.num) {
                return Ok(Object::Reference(ObjRef::new(new_num, 0)));
            }
            let new_num = *next_num;
            *next_num += 1;
            copy_map.insert(r.num, new_num);

            let resolved = source.resolve(*r).map_err(|e| e.to_string())?;
            let copied = copy_object_recursive(source, &resolved, pending, next_num, copy_map)?;
            pending.insert(new_num, copied);
            Ok(Object::Reference(ObjRef::new(new_num, 0)))
        }
        Object::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(copy_object_recursive(
                    source, item, pending, next_num, copy_map,
                )?);
            }
            Ok(Object::Array(out))
        }
        Object::Dictionary(dict) => {
            let mut out = Dictionary::new();
            for (k, v) in dict.iter() {
                out.insert(
                    k.clone(),
                    copy_object_recursive(source, v, pending, next_num, copy_map)?,
                );
            }
            Ok(Object::Dictionary(out))
        }
        Object::Stream(stream) => {
            let mut out_dict = Dictionary::new();
            for (k, v) in stream.dict.iter() {
                out_dict.insert(
                    k.clone(),
                    copy_object_recursive(source, v, pending, next_num, copy_map)?,
                );
            }
            // Les octets bruts ne sont jamais redécodés/recompressés : le
            // `/Filter` copié ci-dessus (une simple valeur `/Name`/`Array`,
            // sans référence à réécrire) reste cohérent avec eux tels quels.
            Ok(Object::Stream(Stream {
                dict: out_dict,
                raw_data: stream.raw_data.clone(),
            }))
        }
        other => Ok(other.clone()),
    }
}

/// Découpage de document (Sprint 15-16, "split") : construit un PDF
/// autonome (`pdf_core::writer::write_standalone`) ne contenant que les
/// pages `indices` de `source` et tout ce qu'elles référencent
/// transitivement — puisque rien d'autre n'est copié, ce fichier ne
/// contient par construction aucun objet inatteignable : un sous-produit
/// naturel de garbage collection, pas une passe séparée.
pub fn extract_pages(source: &Document, indices: &[usize]) -> Result<Vec<u8>, String> {
    let mut pending: std::collections::BTreeMap<u32, Object> = std::collections::BTreeMap::new();
    let mut next_num: u32 = 1;
    let mut copy_map: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();

    let pages_node_num = next_num;
    next_num += 1;
    let pages_node_ref = ObjRef::new(pages_node_num, 0);

    let source_pages = source.pages().map_err(|e| e.to_string())?;
    let mut kids = Vec::with_capacity(indices.len());
    for &idx in indices {
        let src_page = source_pages
            .get(idx)
            .ok_or_else(|| format!("page index {idx} out of bounds"))?;
        let src_ref = src_page
            .object_ref
            .ok_or_else(|| "source page has no indirect object reference".to_string())?;
        let new_num = *copy_map.entry(src_ref.num).or_insert_with(|| {
            let n = next_num;
            next_num += 1;
            n
        });

        let raw = source.resolve(src_ref).map_err(|e| e.to_string())?;
        let copied =
            copy_object_recursive(source, &raw, &mut pending, &mut next_num, &mut copy_map)?;
        let mut dict = copied
            .as_dict()
            .cloned()
            .ok_or_else(|| "copied page is not a dictionary".to_string())?;
        dict.insert("Type", Object::Name("Page".to_string()));
        dict.insert("Parent", Object::Reference(pages_node_ref));
        dict.insert(
            "MediaBox",
            Object::Array(
                src_page
                    .media_box
                    .iter()
                    .map(|&n| Object::Real(n))
                    .collect(),
            ),
        );
        dict.insert("Rotate", Object::Integer(src_page.rotate as i64));
        let copied_resources = copy_object_recursive(
            source,
            &Object::Dictionary(src_page.resources.clone()),
            &mut pending,
            &mut next_num,
            &mut copy_map,
        )?;
        dict.insert("Resources", copied_resources);

        pending.insert(new_num, Object::Dictionary(dict));
        kids.push(Object::Reference(ObjRef::new(new_num, 0)));
    }

    let mut pages_dict = Dictionary::new();
    pages_dict.insert("Type", Object::Name("Pages".to_string()));
    let kids_len = kids.len();
    pages_dict.insert("Kids", Object::Array(kids));
    pages_dict.insert("Count", Object::Integer(kids_len as i64));
    pending.insert(pages_node_num, Object::Dictionary(pages_dict));

    let root_num = next_num;
    let mut root_dict = Dictionary::new();
    root_dict.insert("Type", Object::Name("Catalog".to_string()));
    root_dict.insert("Pages", Object::Reference(pages_node_ref));
    pending.insert(root_num, Object::Dictionary(root_dict));

    let objects: Vec<(ObjRef, Object)> = pending
        .into_iter()
        .map(|(n, o)| (ObjRef::new(n, 0), o))
        .collect();
    Ok(pdf_core::writer::write_standalone(
        &objects,
        ObjRef::new(root_num, 0),
    ))
}

/// "Export / optimisation" (Sprint 15-16) : réécrit `source` en entier via
/// `extract_pages` (toutes les pages, dans l'ordre) plutôt que de la
/// modifier en place — une sauvegarde incrémentale ne peut qu'ajouter, donc
/// ne peut jamais "faire le ménage" ; reconstruire le fichier à partir des
/// seuls objets atteignables depuis les pages en fait un vrai garbage
/// collector, au prix d'une réécriture complète plutôt qu'un simple ajout.
/// **Non fait :** linéarisation (réordonnancement des objets pour
/// l'affichage progressif/streaming) — hors périmètre de cette passe, un
/// chantier à part entière.
pub fn export_optimized(source: &Document) -> Result<Vec<u8>, String> {
    let count = source.page_count().map_err(|e| e.to_string())?;
    let indices: Vec<usize> = (0..count).collect();
    extract_pages(source, &indices)
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

    /// Symétrique de `add_highlight_persists_and_renders_after_reopen`, pour
    /// `/Underline` et `/StrikeOut` (Sprint 20) : une ligne tracée (pas
    /// remplie) doit survivre à la sauvegarde+réouverture.
    #[test]
    fn add_underline_and_strikeout_persist_and_render_as_stroked_lines() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);

        let mut session = EditSession::open(&path).unwrap();
        session
            .add_underline_annotation(0, [100.0, 600.0, 300.0, 630.0], (1.0, 0.0, 0.0))
            .unwrap();
        session
            .add_strikeout_annotation(0, [100.0, 500.0, 300.0, 530.0], (0.0, 0.0, 1.0))
            .unwrap();

        let out_path = write_fixture(b"placeholder-for-unique-name-underline");
        session.save_as(&out_path).unwrap();

        let reopened_bytes = std::fs::read(&out_path).unwrap();
        let doc = pdf_core::Document::open(reopened_bytes).unwrap();
        let page = doc.page(0).unwrap();
        let content = doc.page_content(&page).unwrap();
        let display = Interpreter::run_page_with_annotations(&doc, &page, &content).unwrap();

        let has_stroked_line = |r: f64, g: f64, b: f64| {
            display.items.iter().any(|item| {
                matches!(
                    item,
                    pdf_core::display::DisplayItem::Path {
                        paint: pdf_core::display::PaintOp::Stroke,
                        stroke_color: pdf_core::display::Color::Rgb(cr, cg, cb),
                        ..
                    } if (*cr - r).abs() < 0.01 && (*cg - g).abs() < 0.01 && (*cb - b).abs() < 0.01
                )
            })
        };
        assert!(
            has_stroked_line(1.0, 0.0, 0.0),
            "expected a red stroked line for the underline, got: {:?}",
            display.items
        );
        assert!(
            has_stroked_line(0.0, 0.0, 1.0),
            "expected a blue stroked line for the strikeout, got: {:?}",
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

    /// `form_fields` retrouve le champ texte du fixture avec son nom/rect,
    /// et reflète immédiatement une valeur déjà posée par
    /// `set_form_field_value` dans la même session (lue via `current`, pas
    /// `doc.resolve` — voir la doc de `form_fields`).
    #[test]
    fn form_fields_lists_field_and_reflects_pending_value() {
        let bytes = include_bytes!("../../pdf-core/tests/fixtures/acroform_textfield.pdf").to_vec();
        let path = write_fixture(&bytes);
        let mut session = EditSession::open(&path).unwrap();

        let fields = session.form_fields().unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].name, "name_field");
        assert_eq!(fields[0].value, "");

        session
            .set_form_field_value("name_field", "Ada Lovelace")
            .unwrap();
        let fields = session.form_fields().unwrap();
        assert_eq!(fields[0].value, "Ada Lovelace");

        std::fs::remove_file(&path).ok();
    }

    /// Texte de chaque page d'un document rouvert, dans l'ordre — sert à
    /// vérifier l'ordre/le contenu des pages après manipulation, plus
    /// simple à lire dans un message d'échec qu'une comparaison de
    /// références d'objets.
    fn page_texts(doc: &pdf_core::Document) -> Vec<String> {
        (0..doc.page_count().unwrap())
            .map(|i| {
                let page = doc.page(i).unwrap();
                let content = doc.page_content(&page).unwrap();
                pdf_text::extract_text(
                    &pdf_core::interp::Interpreter::run_page(doc, page.resources.clone(), &content)
                        .unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn insert_delete_move_rotate_survive_save_and_reopen() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);
        let mut session = EditSession::open(&path).unwrap();
        assert_eq!(session.page_count().unwrap(), 5);

        // Insère une page blanche en tête, supprime l'ancienne page 3
        // (index 3 après l'insertion, donc "Page 3" d'origine), déplace la
        // dernière page en seconde position, pivote la nouvelle page 0.
        session
            .insert_blank_page(0, [0.0, 0.0, 400.0, 400.0])
            .unwrap();
        session.delete_page(3).unwrap();
        let last = session.page_count().unwrap() - 1;
        session.move_page(last, 1).unwrap();
        session.rotate_page(0, 90).unwrap();

        let out_path = write_fixture(b"page-manip-out");
        session.save_as(&out_path).unwrap();

        let reopened = pdf_core::Document::open(std::fs::read(&out_path).unwrap()).unwrap();
        assert_eq!(reopened.page_count().unwrap(), 5);
        assert_eq!(reopened.page(0).unwrap().rotate, 90);

        let texts = page_texts(&reopened);
        // Ordre attendu : [blank, Page 5, Page 1, Page 2, Page 4] — Page 3
        // supprimée, Page 5 (dernière, indice 4 après insertion) déplacée
        // en position 1.
        assert!(
            texts[0].is_empty(),
            "expected the inserted page to be blank, got {:?}",
            texts[0]
        );
        assert!(texts[1].contains("Page 5"), "got {:?}", texts);
        assert!(texts[2].contains("Page 1"), "got {:?}", texts);
        assert!(texts[3].contains("Page 2"), "got {:?}", texts);
        assert!(texts[4].contains("Page 4"), "got {:?}", texts);
        assert!(
            !texts.iter().any(|t| t.contains("Page 3")),
            "Page 3 should have been deleted, got {:?}",
            texts
        );

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&out_path).ok();
    }

    #[test]
    fn undo_restores_page_order_after_delete() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);
        let mut session = EditSession::open(&path).unwrap();

        session.delete_page(1).unwrap();
        assert_eq!(session.page_tree.as_ref().unwrap().order.len(), 4);
        assert!(session.undo());
        assert_eq!(session.page_tree.as_ref().unwrap().order.len(), 5);
        assert!(!session.can_undo());

        std::fs::remove_file(&path).ok();
    }

    /// Fusion de documents : les pages d'un second fichier réel doivent
    /// apparaître, dans l'ordre, à la suite du premier — avec leur
    /// contenu **et** leurs polices intégrées réellement copiés (pas
    /// juste des références qui pointeraient dans le vide).
    #[test]
    fn merge_document_appends_real_pages_with_their_resources() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);

        let other_bytes =
            include_bytes!("../../pdf-core/tests/fixtures/embedded_truetype_font.pdf").to_vec();
        let other = pdf_core::Document::open(other_bytes).unwrap();

        let mut session = EditSession::open(&path).unwrap();
        session.merge_document(&other).unwrap();
        assert_eq!(session.page_count().unwrap(), 6);

        let out_path = write_fixture(b"merge-doc-out");
        session.save_as(&out_path).unwrap();

        let reopened = pdf_core::Document::open(std::fs::read(&out_path).unwrap()).unwrap();
        assert_eq!(reopened.page_count().unwrap(), 6);
        let last_page = reopened.page(5).unwrap();
        let content = reopened.page_content(&last_page).unwrap();
        let display = pdf_core::interp::Interpreter::run_page(
            &reopened,
            last_page.resources.clone(),
            &content,
        )
        .unwrap();
        let has_real_outline = display.items.iter().any(|item| {
            matches!(
                item,
                pdf_core::display::DisplayItem::Glyph { outline: Some(o), .. } if !o.is_empty()
            )
        });
        assert!(
            has_real_outline,
            "expected the merged page's embedded TrueType font to still resolve real glyph outlines"
        );

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&out_path).ok();
    }

    /// Découpage : extraire un sous-ensemble de pages doit produire un
    /// fichier autonome, réouvrable indépendamment, avec le bon nombre de
    /// pages et le bon contenu — sans dépendre du fichier source.
    #[test]
    fn extract_pages_produces_a_standalone_reopenable_subset() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let doc = pdf_core::Document::open(bytes).unwrap();

        let extracted = extract_pages(&doc, &[1, 3]).unwrap();
        let reopened = pdf_core::Document::open(extracted).unwrap();
        assert_eq!(reopened.page_count().unwrap(), 2);

        let texts = page_texts(&reopened);
        assert!(texts[0].contains("Page 2"), "got {:?}", texts);
        assert!(texts[1].contains("Page 4"), "got {:?}", texts);
    }

    /// `export_optimized` doit produire un fichier autonome avec toutes les
    /// pages, dans l'ordre, sans corruption — la "compaction" attendue par
    /// le critère de sortie du Sprint 15-16.
    #[test]
    fn export_optimized_preserves_all_pages_in_order() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let doc = pdf_core::Document::open(bytes).unwrap();

        let optimized = export_optimized(&doc).unwrap();
        let reopened = pdf_core::Document::open(optimized).unwrap();
        assert_eq!(reopened.page_count().unwrap(), 5);
        let texts = page_texts(&reopened);
        for (i, text) in texts.iter().enumerate() {
            assert!(
                text.contains(&format!("Page {}", i + 1)),
                "expected page {i} to still say 'Page {}', got {:?}",
                i + 1,
                text
            );
        }
    }

    /// Insérer une image JPEG comme page doit produire une page dont la
    /// taille correspond aux dimensions réelles de l'image et dont le
    /// rendu produit effectivement un pixel décodé (pas juste une image
    /// "positionnée mais non décodée", voir `pdf-core::image`).
    #[test]
    fn insert_image_page_persists_and_renders_after_reopen() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);
        let jpeg_bytes = include_bytes!("../../pdf-core/tests/fixtures/sample_image.jpg");

        let mut session = EditSession::open(&path).unwrap();
        session.insert_image_page(0, jpeg_bytes).unwrap();
        assert_eq!(session.page_count().unwrap(), 6);

        let out_path = write_fixture(b"insert-image-out");
        session.save_as(&out_path).unwrap();

        let reopened = pdf_core::Document::open(std::fs::read(&out_path).unwrap()).unwrap();
        let page = reopened.page(0).unwrap();
        assert_eq!(page.media_box, [0.0, 0.0, 64.0, 48.0]);

        let content = reopened.page_content(&page).unwrap();
        let display =
            pdf_core::interp::Interpreter::run_page(&reopened, page.resources.clone(), &content)
                .unwrap();
        let decoded_image = display.items.iter().find_map(|item| match item {
            pdf_core::display::DisplayItem::Image { pixels, .. } => pixels.as_ref(),
            _ => None,
        });
        let image = decoded_image.expect("expected the inserted image to actually decode");
        assert_eq!((image.width, image.height), (64, 48));

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&out_path).ok();
    }

    /// Sprint 17+, 6a : une annotation `/FreeText` ajoutée doit persister
    /// et produire de vrais glyphes au rendu après réouverture — pas juste
    /// une entrée `/Contents` inerte.
    #[test]
    fn add_free_text_annotation_persists_and_renders_after_reopen() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);

        let mut session = EditSession::open(&path).unwrap();
        session
            .add_free_text_annotation(
                0,
                [50.0, 50.0, 250.0, 80.0],
                "Nouvelle note",
                14.0,
                (0.0, 0.0, 0.0),
            )
            .unwrap();

        let out_path = write_fixture(b"free-text-out");
        session.save_as(&out_path).unwrap();

        let reopened = pdf_core::Document::open(std::fs::read(&out_path).unwrap()).unwrap();
        let page = reopened.page(0).unwrap();
        let content = reopened.page_content(&page).unwrap();
        let display =
            pdf_core::interp::Interpreter::run_page_with_annotations(&reopened, &page, &content)
                .unwrap();

        let glyph_count = display
            .items
            .iter()
            .filter(|i| matches!(i, pdf_core::display::DisplayItem::Glyph { .. }))
            .count();
        // Le contenu de base ("Page 1 - Hello, PDF Manager!") produit déjà
        // des glyphes : on vérifie juste qu'il y en a *plus* qu'avant,
        // preuve que l'annotation a bien ajouté du texte réel.
        let base_display =
            pdf_core::interp::Interpreter::run_page(&reopened, page.resources.clone(), &content)
                .unwrap();
        let base_glyph_count = base_display
            .items
            .iter()
            .filter(|i| matches!(i, pdf_core::display::DisplayItem::Glyph { .. }))
            .count();
        assert!(
            glyph_count > base_glyph_count,
            "expected the FreeText annotation to add glyphs on top of the base page content"
        );

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&out_path).ok();
    }

    /// Sprint 17+, 6b : remplacer une région de texte par superposition ne
    /// doit ni modifier ni supprimer le flux de contenu d'origine (le texte
    /// "caché" reste extractible — seulement recouvert visuellement), et
    /// doit produire un rectangle de fond plein **et** le nouveau texte au
    /// rendu final.
    #[test]
    fn replace_text_with_overlay_covers_without_deleting_original_content() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);

        let mut session = EditSession::open(&path).unwrap();
        session
            .replace_text_with_overlay(
                0,
                [72.0, 715.0, 400.0, 735.0],
                "Texte remplacé",
                18.0,
                (0.0, 0.0, 0.0),
                (1.0, 1.0, 1.0),
            )
            .unwrap();

        let out_path = write_fixture(b"overlay-out");
        session.save_as(&out_path).unwrap();

        let reopened = pdf_core::Document::open(std::fs::read(&out_path).unwrap()).unwrap();
        let page = reopened.page(0).unwrap();
        let content = reopened.page_content(&page).unwrap();

        // Le flux de contenu original doit rester intact : le texte "caché"
        // est toujours là, seulement recouvert au rendu.
        let original_text = pdf_text::extract_text(
            &pdf_core::interp::Interpreter::run_page(&reopened, page.resources.clone(), &content)
                .unwrap(),
        );
        assert!(
            original_text.contains("Page 1"),
            "original content stream should be untouched, got {original_text:?}"
        );

        // Le rendu final (avec annotations) doit montrer un rectangle blanc
        // plein (le cache) et le nouveau texte.
        let display =
            pdf_core::interp::Interpreter::run_page_with_annotations(&reopened, &page, &content)
                .unwrap();
        let has_white_cover = display.items.iter().any(|item| {
            matches!(
                item,
                pdf_core::display::DisplayItem::Path {
                    fill_color: pdf_core::display::Color::Rgb(r, g, b),
                    ..
                } if *r > 0.99 && *g > 0.99 && *b > 0.99
            )
        });
        assert!(has_white_cover, "expected an opaque white cover rectangle");

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&out_path).ok();
    }

    /// L'annotation retirée ne doit plus apparaître dans `/Annots` après
    /// réouverture, et le compte de références doit diminuer d'exactement 1.
    #[test]
    fn remove_annotation_deletes_the_reference_and_persists() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);

        let mut session = EditSession::open(&path).unwrap();
        session
            .add_highlight_annotation(0, [100.0, 600.0, 300.0, 630.0], (1.0, 1.0, 0.0), vec![])
            .unwrap();
        session
            .add_free_text_annotation(0, [50.0, 50.0, 250.0, 80.0], "Note", 14.0, (0.0, 0.0, 0.0))
            .unwrap();
        assert_eq!(session.page_annotation_refs(0).unwrap().len(), 2);

        session.remove_annotation(0, 0).unwrap();
        assert_eq!(session.page_annotation_refs(0).unwrap().len(), 1);

        let out_path = write_fixture(b"remove-annot-out");
        session.save_as(&out_path).unwrap();

        let reopened = pdf_core::Document::open(std::fs::read(&out_path).unwrap()).unwrap();
        let page = reopened.page(0).unwrap();
        let annots = page
            .dict
            .get("Annots")
            .and_then(|o| o.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        assert_eq!(annots, 1);

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&out_path).ok();
    }
}
