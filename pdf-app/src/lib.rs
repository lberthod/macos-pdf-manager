//! État de session applicative : document ouvert, page courante, rendu
//! agnostique du backend de rasterisation — orchestration entre `pdf-core`
//! (parsing/interprétation) et `pdf-render` (rasterisation), pour que le
//! chrome natif macOS (Sprint 11-12) et `pdf-ui` n'aient plus à parler
//! directement à ces deux crates (voir sprint.md, STATUS.md §5).
//!
//! Porte aussi l'édition (Sprints 13-17, `pdf-edit::EditSession`) : chaque
//! opération d'édition régénère immédiatement le `Document` de lecture
//! depuis les octets en attente (`refresh_after_edit`), pour que le rendu
//! reflète l'édition avant toute sauvegarde sur disque — voir
//! `add_highlight_on_current_page`/`undo_edit`/`save_edits_in_place`.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use pdf_core::{Document, Object};

/// Ré-exporté tel quel (pas de conversion vers un type propre à `pdf-app`,
/// contrairement à `MatchRect`) : `OutlineItem` ne porte aucune notion liée
/// au backend de rendu, juste un titre/index de page/enfants.
pub use pdf_core::OutlineItem;

/// Nombre maximal de pages rastérisées gardées en mémoire (`render_cache`) :
/// couvre à la fois la page courante et un jeu de miniatures sans borner la
/// mémoire pour un document de plusieurs centaines de pages.
const RENDER_CACHE_CAPACITY: usize = 32;

/// Clé `(page_index, échelle quantifiée)` du cache de rendu, voir
/// `Session::render_cache`.
type RenderCacheKey = (usize, u32);

/// Image RGBA8 déjà rastérisée pour une page, indépendante du backend de
/// rendu (`tiny-skia` aujourd'hui, potentiellement `wgpu` au Sprint 9-10) :
/// seuls `width`/`height`/`rgba` traversent la frontière de `pdf-app`.
/// `rgba` est au format produit par `tiny_skia::Pixmap::data()` (prémultiplié).
pub struct RenderedPage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Rectangle englobant (espace page PDF, origine bas-gauche — comme
/// `pdf_core::page::Page::media_box`) d'une occurrence de recherche trouvée
/// sur une page. Type propre à `pdf-app` (plutôt que de ré-exporter
/// `pdf_text::GlyphRect`) pour que `pdf-ui` n'ait pas besoin de dépendre de
/// `pdf-text` directement — voir architecture.md §8.1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MatchRect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl From<pdf_text::GlyphRect> for MatchRect {
    fn from(r: pdf_text::GlyphRect) -> Self {
        Self {
            x0: r.x0,
            y0: r.y0,
            x1: r.x1,
            y1: r.y1,
        }
    }
}

/// Annotation visible d'une page (Sprint 20) : juste assez d'information
/// pour que `pdf-ui` puisse dessiner son contour et proposer de la
/// supprimer (`Session::remove_annotation_on_current_page`, déjà existant)
/// — pas une copie complète du dictionnaire d'annotation. `index` est la
/// position dans `/Annots`, celle attendue par `remove_annotation`.
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotationInfo {
    pub index: usize,
    pub rect: [f64; 4],
    /// `/Subtype` (`"Highlight"`, `"FreeText"`, `"Underline"`,
    /// `"StrikeOut"`...), chaîne vide si absent.
    pub subtype: String,
}

/// Comme `Object::as_int`/`as_real` mais accepte indifféremment `Integer` et
/// `Real` — les `/Rect`/`/BBox` d'un PDF réel mélangent les deux (voir la
/// même nécessité dans `pdf_core::interp::obj_num`, privée à ce crate).
fn obj_num(o: &Object) -> f64 {
    match o {
        Object::Integer(n) => *n as f64,
        Object::Real(f) => *f,
        _ => 0.0,
    }
}

/// Un document PDF ouvert avec sa position de lecture courante (page).
pub struct Session {
    doc: Document,
    path: PathBuf,
    page_count: usize,
    page_index: usize,
    /// Texte (avec positions) déjà extrait par page (`None` = pas encore
    /// demandé). Évite de ré-interpréter le flux de contenu d'une page déjà
    /// vue à chaque appel de `find_pages_containing`/
    /// `find_matches_on_current_page` — le texte d'une page ne change jamais
    /// pendant une session de lecture, donc rien à invalider. `RefCell` : ce
    /// cache est un détail d'implémentation, porté par des méthodes `&self`.
    text_cache: RefCell<Vec<Option<Rc<pdf_text::PageText>>>>,
    /// Cache FIFO des dernières pages rastérisées, clé `(page_index,
    /// échelle quantifiée)` : réutilisé par `render_page`/
    /// `render_current_page`, en particulier pour les miniatures (beaucoup
    /// de pages, échelle fixe) et la navigation aller-retour. Pas un
    /// vrai cache de tuiles GPU (voir sprint.md Sprint 9-10) : une page
    /// entière est mise en cache, pas des dalles.
    render_cache: RefCell<Vec<(RenderCacheKey, Rc<RenderedPage>)>>,
    /// Table des matières, calculée au premier appel de `outline()` (`None`
    /// = pas encore demandée) : ne change jamais pendant une session.
    outline_cache: RefCell<Option<Rc<Vec<OutlineItem>>>>,
    /// Backend GPU optionnel (voir `set_gpu_renderer`) : `None` par défaut
    /// (rendu `pdf-render`/tiny-skia uniquement). Quand présent, `render_page`
    /// l'essaie en premier et ne se rabat sur `pdf-render` que s'il échoue
    /// (pas d'adaptateur `wgpu` compatible) — voir `pdf_render_gpu::GpuRenderer`.
    gpu: Option<pdf_render_gpu::GpuRenderer>,
    /// Session d'édition (Sprints 13-17, `pdf-edit`) : annotations,
    /// formulaires, pages, undo/redo, sauvegarde incrémentale. Ouverte sur
    /// le même fichier que `doc`, mais un objet `Document` distinct (lecture
    /// seule, jamais modifié directement) — après chaque édition, `doc` est
    /// entièrement rouvert depuis `edit.to_bytes()` (voir
    /// `refresh_after_edit`) pour que le rendu reflète immédiatement les
    /// modifications en attente, sans jamais toucher au fichier sur disque
    /// tant que l'utilisateur ne sauvegarde pas explicitement.
    edit: pdf_edit::EditSession,
}

impl Session {
    /// Ouvre un fichier PDF depuis le disque.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        let doc = Document::open(bytes).map_err(|e| e.to_string())?;
        let page_count = doc.page_count().map_err(|e| e.to_string())?;
        let edit = pdf_edit::EditSession::open(&path)?;
        Ok(Self {
            doc,
            path,
            page_count,
            page_index: 0,
            text_cache: RefCell::new(vec![None; page_count]),
            render_cache: RefCell::new(Vec::new()),
            outline_cache: RefCell::new(None),
            gpu: None,
            edit,
        })
    }

    /// Active le backend GPU (`pdf-render-gpu`) pour les rendus suivants de
    /// cette session — typiquement appelé une fois juste après `open()` avec
    /// un `GpuRenderer` construit à partir du `Device`/`Queue` déjà négocié
    /// par l'hôte (voir `pdf_render_gpu::GpuRenderer::from_shared`, et
    /// `pdf-ui` pour l'intégration `eframe`). Sans cet appel, `render_page`
    /// continue d'utiliser `pdf-render` (tiny-skia) comme avant.
    pub fn set_gpu_renderer(&mut self, gpu: pdf_render_gpu::GpuRenderer) {
        self.gpu = Some(gpu);
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn page_count(&self) -> usize {
        self.page_count
    }

    pub fn page_index(&self) -> usize {
        self.page_index
    }

    /// Change de page si `index` est dans les bornes ; ignoré silencieusement
    /// sinon (à l'appelant de désactiver la navigation via `page_count`).
    pub fn goto_page(&mut self, index: usize) {
        if index < self.page_count {
            self.page_index = index;
        }
    }

    pub fn next_page(&mut self) {
        self.goto_page(self.page_index + 1);
    }

    pub fn prev_page(&mut self) {
        if self.page_index > 0 {
            self.page_index -= 1;
        }
    }

    /// `MediaBox` de la page courante (espace page PDF), nécessaire à
    /// l'appelant pour convertir les rectangles de `find_matches_on_current_page`
    /// en coordonnées écran.
    pub fn current_page_media_box(&self) -> Result<[f64; 4], String> {
        self.page_media_box(self.page_index)
    }

    /// `MediaBox` de `index`, indépendamment de la page courante (utilisé
    /// pour dimensionner les lignes du défilement continu).
    pub fn page_media_box(&self, index: usize) -> Result<[f64; 4], String> {
        Ok(self.doc.page(index).map_err(|e| e.to_string())?.media_box)
    }

    /// Table des matières (`/Outlines`) du document, `[]` si absente. Mise
    /// en cache après le premier appel (voir `pdf_core::outline` pour les
    /// limitations : seules les destinations directes sont résolues).
    pub fn outline(&self) -> Result<Rc<Vec<OutlineItem>>, String> {
        if let Some(cached) = self.outline_cache.borrow().clone() {
            return Ok(cached);
        }
        let outline = Rc::new(self.doc.outline().map_err(|e| e.to_string())?);
        *self.outline_cache.borrow_mut() = Some(outline.clone());
        Ok(outline)
    }

    /// Rastérise la page courante à l'échelle `scale`, `/Rotate` appliqué
    /// (voir `pdf_render::render_page_rotated`). Mis en cache par `(page,
    /// échelle)`.
    pub fn render_current_page(&self, scale: f64) -> Result<Rc<RenderedPage>, String> {
        self.render_page(self.page_index, scale)
    }

    /// Rastérise `index` à l'échelle `scale`, indépendamment de la page
    /// courante (utilisé par les miniatures). Mis en cache par `(index,
    /// échelle quantifiée)` : un second appel avec les mêmes paramètres ne
    /// re-rastérise pas.
    pub fn render_page(&self, index: usize, scale: f64) -> Result<Rc<RenderedPage>, String> {
        let scale_key = (scale * 1000.0).round() as u32;
        let key = (index, scale_key);
        if let Some((_, cached)) = self.render_cache.borrow().iter().find(|(k, _)| *k == key) {
            return Ok(cached.clone());
        }

        let (page, display) = self.page_display(index)?;
        let gpu_rendered = self.gpu.as_ref().and_then(|gpu| {
            gpu.render_page_rotated(&display, page.media_box, page.rotate, scale)
                .map(|p| RenderedPage {
                    width: p.width,
                    height: p.height,
                    rgba: p.rgba,
                })
        });
        let rendered = Rc::new(match gpu_rendered {
            Some(rendered) => rendered,
            None => {
                let pixmap =
                    pdf_render::render_page_rotated(&display, page.media_box, page.rotate, scale)
                        .ok_or_else(|| "render target allocation failed".to_string())?;
                RenderedPage {
                    width: pixmap.width(),
                    height: pixmap.height(),
                    rgba: pixmap.data().to_vec(),
                }
            }
        });

        let mut cache = self.render_cache.borrow_mut();
        if cache.len() >= RENDER_CACHE_CAPACITY {
            cache.remove(0); // FIFO : évince l'entrée la plus ancienne.
        }
        cache.push((key, rendered.clone()));
        Ok(rendered)
    }

    /// Texte de la page courante (voir `pdf_text::extract_text` pour les
    /// limites de la reconstruction : pas de blocs/colonnes, glyphes non
    /// résolus en Unicode omis). Mis en cache après le premier appel.
    pub fn extract_current_page_text(&self) -> Result<String, String> {
        Ok(self.cached_page_text(self.page_index)?.text().to_string())
    }

    /// Texte de tout le document, une page par section séparée par un saut de
    /// page (`\x0c`) — utilisé par l'export `.txt` (Sprint 18). Réutilise le
    /// même cache par page que `find_pages_containing`/
    /// `extract_current_page_text` : n'importe quelle page déjà vue n'est pas
    /// ré-extraite.
    pub fn extract_all_text(&self) -> Result<String, String> {
        let mut out = String::new();
        for index in 0..self.page_count {
            if index > 0 {
                out.push('\x0c');
                out.push('\n');
            }
            out.push_str(self.cached_page_text(index)?.text());
        }
        Ok(out)
    }

    /// Recherche `query` (insensible à la casse **et aux accents**, voir
    /// `pdf_text::normalize_for_search`) dans le texte de chaque page et
    /// retourne les index de page où au moins une occurrence a été trouvée.
    /// Le texte de chaque page n'est extrait qu'une fois (voir `text_cache`) :
    /// une deuxième recherche sur le même document ne ré-interprète aucun
    /// flux de contenu.
    pub fn find_pages_containing(&self, query: &str) -> Result<Vec<usize>, String> {
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let query = pdf_text::normalize_for_search(query);
        let mut matches = Vec::new();
        for index in 0..self.page_count {
            let haystack = pdf_text::normalize_for_search(self.cached_page_text(index)?.text());
            if haystack.contains(&query) {
                matches.push(index);
            }
        }
        Ok(matches)
    }

    /// Rectangles (espace page PDF) de chaque occurrence de `query` sur la
    /// page courante — voir `pdf_text::PageText::find_matches` pour les
    /// limites (repliement de casse caractère par caractère, pas de
    /// reconstruction par blocs).
    pub fn find_matches_on_current_page(&self, query: &str) -> Result<Vec<MatchRect>, String> {
        let page_text = self.cached_page_text(self.page_index)?;
        Ok(page_text
            .find_matches(query)
            .into_iter()
            .map(MatchRect::from)
            .collect())
    }

    /// Indice de caractère (page courante) le plus proche de `point` (espace
    /// page PDF), pour la sélection de texte à la souris — voir
    /// `pdf_text::PageText::char_index_at`.
    pub fn char_index_at_on_current_page(
        &self,
        point: (f64, f64),
    ) -> Result<Option<usize>, String> {
        Ok(self.cached_page_text(self.page_index)?.char_index_at(point))
    }

    /// Plage de caractères (page courante) du "mot" contenant `index` — voir
    /// `pdf_text::PageText::word_range_at`, utilisé pour le double-clic.
    pub fn word_range_at_on_current_page(
        &self,
        index: usize,
    ) -> Result<std::ops::Range<usize>, String> {
        Ok(self.cached_page_text(self.page_index)?.word_range_at(index))
    }

    /// Plage de caractères (page courante) de la "ligne" contenant `index` —
    /// voir `pdf_text::PageText::line_range_at`, utilisé pour le triple-clic.
    pub fn line_range_at_on_current_page(
        &self,
        index: usize,
    ) -> Result<std::ops::Range<usize>, String> {
        Ok(self.cached_page_text(self.page_index)?.line_range_at(index))
    }

    /// Texte et rectangles (espace page PDF, un par caractère, non fusionnés
    /// contrairement à `find_matches_on_current_page`) de `range` sur la
    /// page courante — utilisé pour surligner et copier une sélection de
    /// texte à la souris.
    pub fn selection_on_current_page(
        &self,
        range: std::ops::Range<usize>,
    ) -> Result<(String, Vec<MatchRect>), String> {
        let page_text = self.cached_page_text(self.page_index)?;
        let text = page_text.text_in_range(range.clone());
        let rects = page_text
            .rects_in_range(range)
            .into_iter()
            .map(MatchRect::from)
            .collect();
        Ok((text, rects))
    }

    /// `true` si `undo`/`redo` a quelque chose à défaire/refaire — sert à
    /// `pdf-ui` pour activer/désactiver les boutons et items de menu
    /// correspondants sans dupliquer l'état de `pdf-edit`.
    pub fn can_undo_edit(&self) -> bool {
        self.edit.can_undo()
    }

    pub fn can_redo_edit(&self) -> bool {
        self.edit.can_redo()
    }

    /// Ajoute une annotation `/Highlight` sur la page courante (voir
    /// `pdf_edit::EditSession::add_highlight_annotation`) et rafraîchit
    /// immédiatement le rendu pour qu'elle soit visible sans sauvegarder.
    pub fn add_highlight_on_current_page(
        &mut self,
        rect: [f64; 4],
        color: (f32, f32, f32),
    ) -> Result<(), String> {
        self.edit
            .add_highlight_annotation(self.page_index, rect, color, vec![])?;
        self.refresh_after_edit()
    }

    /// Ajoute une annotation `/FreeText` (Sprint 17+, 6a) sur la page
    /// courante et rafraîchit le rendu.
    pub fn add_free_text_on_current_page(
        &mut self,
        rect: [f64; 4],
        text: &str,
        font_size: f64,
        color: (f32, f32, f32),
    ) -> Result<(), String> {
        self.edit
            .add_free_text_annotation(self.page_index, rect, text, font_size, color)?;
        self.refresh_after_edit()
    }

    /// Souligne la sélection de texte courante (Sprint 20, voir
    /// `pdf_edit::EditSession::add_underline_annotation`).
    pub fn add_underline_on_current_page(
        &mut self,
        rect: [f64; 4],
        color: (f32, f32, f32),
    ) -> Result<(), String> {
        self.edit
            .add_underline_annotation(self.page_index, rect, color)?;
        self.refresh_after_edit()
    }

    /// Barre la sélection de texte courante (Sprint 20, voir
    /// `pdf_edit::EditSession::add_strikeout_annotation`).
    pub fn add_strikeout_on_current_page(
        &mut self,
        rect: [f64; 4],
        color: (f32, f32, f32),
    ) -> Result<(), String> {
        self.edit
            .add_strikeout_annotation(self.page_index, rect, color)?;
        self.refresh_after_edit()
    }

    /// Remplace (par superposition : masque + redessine, voir
    /// `pdf_edit::EditSession::replace_text_with_overlay`) le texte de la
    /// page courante situé dans `rect` par `text` (Sprint 20, câblage UI de
    /// l'édition de texte 6b).
    pub fn replace_text_on_current_page(
        &mut self,
        rect: [f64; 4],
        text: &str,
        font_size: f64,
        text_color: (f32, f32, f32),
        background: (f32, f32, f32),
    ) -> Result<(), String> {
        self.edit.replace_text_with_overlay(
            self.page_index,
            rect,
            text,
            font_size,
            text_color,
            background,
        )?;
        self.refresh_after_edit()
    }

    /// Retire l'annotation d'indice `annot_index` de la page courante (voir
    /// `pdf_edit::EditSession::remove_annotation`) et rafraîchit le rendu.
    pub fn remove_annotation_on_current_page(&mut self, annot_index: usize) -> Result<(), String> {
        self.edit.remove_annotation(self.page_index, annot_index)?;
        self.refresh_after_edit()
    }

    /// Liste les annotations visibles de la page courante (Sprint 20) — même
    /// filtre `Hidden`/`NoView` que le rendu normal
    /// (`pdf_core::interp::run_page_with_annotations`), pour que `pdf-ui`
    /// puisse afficher un contour cliquable par annotation et proposer de la
    /// supprimer (`remove_annotation_on_current_page`, l'`index` renvoyé ici
    /// est directement celui attendu par cette méthode).
    pub fn annotations_on_current_page(&self) -> Result<Vec<AnnotationInfo>, String> {
        let page = self.doc.page(self.page_index).map_err(|e| e.to_string())?;
        let Some(annots_obj) = page.dict.get("Annots") else {
            return Ok(Vec::new());
        };
        let annots = self.doc.get(annots_obj).map_err(|e| e.to_string())?;
        let Some(refs) = annots.as_array() else {
            return Ok(Vec::new());
        };

        let mut out = Vec::new();
        for (index, annot_ref) in refs.iter().enumerate() {
            let Ok(annot_obj) = self.doc.get(annot_ref) else {
                continue;
            };
            let Some(dict) = annot_obj.as_dict() else {
                continue;
            };
            if let Some(flags) = dict.get("F").and_then(|o| o.as_int()) {
                if flags & 0x2 != 0 || flags & 0x20 != 0 {
                    continue; // Hidden ou NoView, ISO 32000-1 §12.5.3 tableau 165.
                }
            }
            let Some(rect_arr) = dict
                .get("Rect")
                .and_then(|o| o.as_array())
                .filter(|r| r.len() >= 4)
            else {
                continue;
            };
            let rect = [
                obj_num(&rect_arr[0]),
                obj_num(&rect_arr[1]),
                obj_num(&rect_arr[2]),
                obj_num(&rect_arr[3]),
            ];
            let subtype = dict
                .get("Subtype")
                .and_then(|o| o.as_name())
                .unwrap_or("")
                .to_string();
            out.push(AnnotationInfo {
                index,
                rect,
                subtype,
            });
        }
        Ok(out)
    }

    /// Liste les champs de formulaire texte de la page courante (voir
    /// `pdf_edit::EditSession::form_fields`) — filtré à partir des `/Annots`
    /// de la page courante (même source que `annotations_on_current_page`)
    /// puisque `form_fields` liste tous les champs du document sans
    /// distinction de page. Permet à `pdf-ui` de dessiner un contour
    /// cliquable par champ et de préremplir une modale de saisie avec la
    /// valeur actuelle.
    pub fn form_fields_on_current_page(&self) -> Result<Vec<pdf_edit::FormFieldInfo>, String> {
        let page = self.doc.page(self.page_index).map_err(|e| e.to_string())?;
        let Some(annots_obj) = page.dict.get("Annots") else {
            return Ok(Vec::new());
        };
        let annots = self.doc.get(annots_obj).map_err(|e| e.to_string())?;
        let Some(refs) = annots.as_array() else {
            return Ok(Vec::new());
        };
        let page_nums: std::collections::HashSet<u32> = refs
            .iter()
            .filter_map(|o| match o {
                Object::Reference(r) => Some(r.num),
                _ => None,
            })
            .collect();

        Ok(self
            .edit
            .form_fields()?
            .into_iter()
            .filter(|f| page_nums.contains(&f.obj_ref.num))
            .collect())
    }

    /// Fixe la valeur du champ de formulaire texte `field_name` (voir
    /// `pdf_edit::EditSession::set_form_field_value`) et rafraîchit
    /// immédiatement le rendu pour que la nouvelle valeur soit visible
    /// avant toute sauvegarde.
    pub fn set_form_field_value_on_current_page(
        &mut self,
        field_name: &str,
        value: &str,
    ) -> Result<(), String> {
        self.edit.set_form_field_value(field_name, value)?;
        self.refresh_after_edit()
    }

    /// Insère une page blanche à `at_index` (Sprint 19, câblage `pdf-ui` de
    /// `pdf_edit::EditSession::insert_blank_page`) — reprend le `MediaBox` de
    /// la page courante si le document en a au moins une, sinon une page
    /// Lettre US par défaut (612×792pt).
    pub fn insert_blank_page_at(&mut self, at_index: usize) -> Result<(), String> {
        let media_box = self
            .current_page_media_box()
            .unwrap_or([0.0, 0.0, 612.0, 792.0]);
        self.edit.insert_blank_page(at_index, media_box)?;
        self.refresh_after_edit()
    }

    /// Insère une nouvelle page à `at_index` dont le contenu est l'image
    /// JPEG `jpeg_bytes` (voir `pdf_edit::EditSession::insert_image_page` :
    /// JPEG seulement, intégré tel quel).
    pub fn insert_image_page_at(
        &mut self,
        at_index: usize,
        jpeg_bytes: &[u8],
    ) -> Result<(), String> {
        self.edit.insert_image_page(at_index, jpeg_bytes)?;
        self.refresh_after_edit()
    }

    /// Supprime la page `index` (voir `pdf_edit::EditSession::delete_page`).
    pub fn delete_page_at(&mut self, index: usize) -> Result<(), String> {
        self.edit.delete_page(index)?;
        self.refresh_after_edit()
    }

    /// Déplace la page `from` à la position `to` (voir
    /// `pdf_edit::EditSession::move_page`) — utilisé par le glisser-déposer
    /// des miniatures.
    pub fn move_page(&mut self, from: usize, to: usize) -> Result<(), String> {
        self.edit.move_page(from, to)?;
        self.refresh_after_edit()
    }

    /// Ajoute `delta` degrés (multiple de 90) à la rotation persistée de la
    /// page `index` (voir `pdf_edit::EditSession::rotate_page` — distinct
    /// d'une rotation de vue éphémère, qui n'existe pas dans ce viewer).
    pub fn rotate_page_at(&mut self, index: usize, delta: i32) -> Result<(), String> {
        self.edit.rotate_page(index, delta)?;
        self.refresh_after_edit()
    }

    /// Concatène la totalité du PDF situé à `source_path` à la fin du
    /// document courant (voir `pdf_edit::EditSession::merge_document`) —
    /// ouvre `source_path` en lecture seule, ne modifie jamais ce fichier.
    pub fn merge_document_from_path(&mut self, source_path: &Path) -> Result<(), String> {
        let bytes = std::fs::read(source_path).map_err(|e| e.to_string())?;
        let source = Document::open(bytes).map_err(|e| e.to_string())?;
        self.edit.merge_document(&source)?;
        self.refresh_after_edit()
    }

    /// Extrait `indices` du document courant (état actuel, y compris les
    /// éditions en attente) vers un nouveau fichier autonome `dest` (voir
    /// `pdf_edit::extract_pages`) — ne modifie pas la session en cours, sert
    /// au "découper"/"extraire une sélection de pages".
    pub fn extract_pages_to_file(&self, indices: &[usize], dest: &Path) -> Result<(), String> {
        let bytes = pdf_edit::extract_pages(&self.doc, indices)?;
        std::fs::write(dest, bytes).map_err(|e| e.to_string())
    }

    /// Défait la dernière opération d'édition et rafraîchit le rendu.
    /// Ne fait rien (renvoie `false`) s'il n'y a rien à défaire.
    pub fn undo_edit(&mut self) -> Result<bool, String> {
        if !self.edit.undo() {
            return Ok(false);
        }
        self.refresh_after_edit()?;
        Ok(true)
    }

    /// Refait la dernière opération défaite et rafraîchit le rendu.
    pub fn redo_edit(&mut self) -> Result<bool, String> {
        if !self.edit.redo() {
            return Ok(false);
        }
        self.refresh_after_edit()?;
        Ok(true)
    }

    /// Écrit les modifications en attente **dans le fichier actuellement
    /// ouvert** (`self.path`) par sauvegarde incrémentale — un vrai
    /// "Enregistrer" plutôt qu'un "Enregistrer sous" : le fichier d'origine
    /// n'est jamais réécrit en place au sens strict (voir
    /// `Document::save_incremental`), seulement complété, mais du point de
    /// vue de l'utilisateur c'est bien la même session de fichier qui est
    /// mise à jour.
    pub fn save_edits_in_place(&self) -> Result<(), String> {
        self.edit.save_as(&self.path)
    }

    /// Octets du document courant, éditions en attente incluses (voir
    /// `pdf_edit::EditSession::to_bytes`) — sans toucher au fichier sur
    /// disque. Utilisé pour imprimer "tel qu'affiché" (Sprint 21, #48) via
    /// un fichier temporaire, sans attendre un `save_edits_in_place`.
    pub fn current_bytes(&self) -> Result<Vec<u8>, String> {
        self.edit.to_bytes()
    }

    /// Octets d'une version optimisée du document courant (Sprint 22, #45) :
    /// un vrai garbage collector par reconstruction (`pdf_edit::export_optimized`,
    /// ne copie que les objets atteignables depuis les pages) — élimine les
    /// objets orphelins laissés par `undo`/les éditions successives (voir la
    /// limitation documentée sur `undo_edit`). Opère sur `self.doc`, donc
    /// inclut déjà toute édition en attente (`refresh_after_edit` le tient à
    /// jour).
    pub fn export_optimized(&self) -> Result<Vec<u8>, String> {
        pdf_edit::export_optimized(&self.doc)
    }

    /// Après toute opération d'édition : régénère les octets du document
    /// (avec les modifications en attente) via `pdf_edit::EditSession::
    /// to_bytes`, rouvre un `Document` en lecture depuis ces octets pour le
    /// rendu/la navigation, et invalide tous les caches (rendu, texte, plan)
    /// puisqu'ils peuvent référencer un contenu désormais périmé. Le
    /// fichier sur disque n'est jamais touché ici — seul `save_edits_in_place`
    /// écrit réellement.
    fn refresh_after_edit(&mut self) -> Result<(), String> {
        let bytes = self.edit.to_bytes()?;
        let doc = Document::open(bytes).map_err(|e| e.to_string())?;
        self.page_count = doc.page_count().map_err(|e| e.to_string())?;
        self.page_index = self.page_index.min(self.page_count.saturating_sub(1));
        self.doc = doc;
        self.render_cache.borrow_mut().clear();
        self.text_cache.replace(vec![None; self.page_count]);
        self.outline_cache.borrow_mut().take();
        Ok(())
    }

    /// Retourne le texte (avec positions) de `index`, en le calculant et le
    /// mémorisant dans `text_cache` si c'est la première demande pour cette
    /// page.
    fn cached_page_text(&self, index: usize) -> Result<Rc<pdf_text::PageText>, String> {
        if let Some(cached) = self.text_cache.borrow()[index].clone() {
            return Ok(cached);
        }
        let (_page, display) = self.page_display(index)?;
        let page_text = Rc::new(pdf_text::extract_page_text(&display));
        self.text_cache.borrow_mut()[index] = Some(page_text.clone());
        Ok(page_text)
    }

    fn page_display(
        &self,
        index: usize,
    ) -> Result<(pdf_core::Page, pdf_core::display::DisplayList), String> {
        let page = self.doc.page(index).map_err(|e| e.to_string())?;
        let content = self.doc.page_content(&page).map_err(|e| e.to_string())?;
        // `run_page_with_annotations` plutôt que `run_page` : les annotations
        // (surlignages, notes, champs de formulaire remplis — Sprint 13-14)
        // doivent apparaître dans le rendu normal du viewer, pas seulement
        // dans un chemin de test dédié.
        let display =
            pdf_core::interp::Interpreter::run_page_with_annotations(&self.doc, &page, &content)
                .map_err(|e| e.to_string())?;
        Ok((page, display))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_fixture(bytes: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "pdf_app_test_{}_{:p}.pdf",
            std::process::id(),
            bytes
        ));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn open_reports_page_count_and_starts_at_first_page() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);
        let session = Session::open(&path).unwrap();
        assert_eq!(session.page_count(), 5);
        assert_eq!(session.page_index(), 0);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn navigation_clamps_at_document_bounds() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);
        let mut session = Session::open(&path).unwrap();

        session.prev_page();
        assert_eq!(session.page_index(), 0, "can't go before the first page");

        for _ in 0..10 {
            session.next_page();
        }
        assert_eq!(
            session.page_index(),
            session.page_count() - 1,
            "can't go past the last page"
        );

        session.goto_page(2);
        assert_eq!(session.page_index(), 2);
        session.goto_page(999);
        assert_eq!(session.page_index(), 2, "out-of-bounds goto is ignored");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn render_current_page_produces_matching_pixel_buffer() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);
        let session = Session::open(&path).unwrap();

        let rendered = session.render_current_page(1.0).unwrap();
        assert_eq!(
            rendered.rgba.len(),
            (rendered.width * rendered.height * 4) as usize
        );
        assert!(rendered.width > 0 && rendered.height > 0);

        std::fs::remove_file(&path).ok();
    }

    /// Quand un `GpuRenderer` est attaché (voir `set_gpu_renderer`),
    /// `render_page` doit produire une page de même forme (largeur/hauteur
    /// cohérentes avec le buffer RGBA) que le chemin `pdf-render` par
    /// défaut — que le rendu ait réellement utilisé le GPU (adaptateur
    /// disponible) ou soit retombé sur `pdf-render` (pas d'adaptateur dans
    /// cet environnement, voir `GpuRenderer::new`), l'appelant ne doit rien
    /// pouvoir en distinguer côté forme du résultat.
    #[test]
    fn render_page_with_gpu_renderer_attached_produces_a_usable_page() {
        let Some(gpu) = pdf_render_gpu::GpuRenderer::new() else {
            eprintln!("no wgpu adapter available in this environment, skipping");
            return;
        };
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);
        let mut session = Session::open(&path).unwrap();
        session.set_gpu_renderer(gpu);

        let rendered = session.render_current_page(1.0).unwrap();
        assert_eq!(
            rendered.rgba.len(),
            (rendered.width * rendered.height * 4) as usize
        );
        assert!(rendered.width > 0 && rendered.height > 0);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn open_missing_file_returns_error() {
        let result = Session::open("/nonexistent/path/does-not-exist.pdf");
        assert!(result.is_err());
    }

    #[test]
    fn extract_current_page_text_follows_navigation() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);
        let mut session = Session::open(&path).unwrap();

        assert_eq!(
            session.extract_current_page_text().unwrap(),
            "Page 1 - Hello, PDF Manager!"
        );
        session.goto_page(2);
        assert_eq!(
            session.extract_current_page_text().unwrap(),
            "Page 3 - Hello, PDF Manager!"
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn find_pages_containing_is_case_insensitive_and_searches_every_page() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);
        let session = Session::open(&path).unwrap();

        assert_eq!(
            session.find_pages_containing("hello").unwrap(),
            vec![0, 1, 2, 3, 4]
        );
        assert_eq!(session.find_pages_containing("Page 3").unwrap(), vec![2]);
        assert_eq!(
            session.find_pages_containing("nonexistent").unwrap(),
            Vec::<usize>::new()
        );
        assert_eq!(
            session.find_pages_containing("").unwrap(),
            Vec::<usize>::new()
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn repeated_searches_reuse_the_text_cache() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);
        let session = Session::open(&path).unwrap();

        assert!(session
            .text_cache
            .borrow()
            .iter()
            .all(|cached| cached.is_none()));

        session.find_pages_containing("hello").unwrap();
        assert!(
            session
                .text_cache
                .borrow()
                .iter()
                .all(|cached| cached.is_some()),
            "every page should be cached after a full-document search"
        );

        // Une deuxième recherche doit retomber sur le cache et donner le
        // même résultat, sans avoir besoin de ré-ouvrir le document.
        assert_eq!(session.find_pages_containing("Page 5").unwrap(), vec![4]);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn find_matches_on_current_page_locates_the_search_term() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);
        let session = Session::open(&path).unwrap();

        let matches = session.find_matches_on_current_page("hello").unwrap();
        assert_eq!(matches.len(), 1);
        assert!(matches[0].x1 > matches[0].x0);

        assert!(session
            .find_matches_on_current_page("nonexistent")
            .unwrap()
            .is_empty());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn repeated_renders_of_the_same_page_and_scale_reuse_the_cache() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);
        let session = Session::open(&path).unwrap();

        let first = session.render_page(0, 1.0).unwrap();
        let second = session.render_page(0, 1.0).unwrap();
        assert!(
            Rc::ptr_eq(&first, &second),
            "second render of the same (page, scale) should hit the cache, not re-rasterize"
        );

        let different_scale = session.render_page(0, 2.0).unwrap();
        assert!(
            !Rc::ptr_eq(&first, &different_scale),
            "a different scale must not reuse a cached render for another scale"
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn current_page_media_box_matches_the_fixture_page_size() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);
        let session = Session::open(&path).unwrap();

        let media_box = session.current_page_media_box().unwrap();
        assert_eq!(media_box, [0.0, 0.0, 612.0, 792.0]);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn outline_is_read_and_cached() {
        let bytes = include_bytes!("../../pdf-core/tests/fixtures/outline_test.pdf").to_vec();
        let path = write_fixture(&bytes);
        let session = Session::open(&path).unwrap();

        let outline = session.outline().unwrap();
        assert_eq!(outline.len(), 4);
        assert_eq!(outline[2].title, "Section 3");
        assert_eq!(outline[2].page_index, Some(2));

        // Deuxième appel : doit retomber sur le cache (même Rc).
        let outline_again = session.outline().unwrap();
        assert!(Rc::ptr_eq(&outline, &outline_again));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn outline_is_empty_for_a_document_without_one() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);
        let session = Session::open(&path).unwrap();

        assert!(session.outline().unwrap().is_empty());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn selection_on_current_page_returns_the_requested_text_and_rects() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);
        let session = Session::open(&path).unwrap();

        // "Page 1 - Hello, PDF Manager!" -> "Hello" commence à l'indice 9.
        let (text, rects) = session.selection_on_current_page(9..14).unwrap();
        assert_eq!(text, "Hello");
        assert_eq!(rects.len(), 5);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn char_index_at_on_current_page_hits_a_known_character() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);
        let session = Session::open(&path).unwrap();

        let (_, rects) = session.selection_on_current_page(0..1).unwrap();
        let first_char_rect = rects[0];
        let center = (
            (first_char_rect.x0 + first_char_rect.x1) / 2.0,
            (first_char_rect.y0 + first_char_rect.y1) / 2.0,
        );
        assert_eq!(
            session.char_index_at_on_current_page(center).unwrap(),
            Some(0)
        );

        std::fs::remove_file(&path).ok();
    }

    /// Une annotation ajoutée doit être visible dans le rendu **immédiatement**
    /// (avant toute sauvegarde) — c'est tout l'intérêt de `refresh_after_edit` :
    /// sans lui, l'utilisateur ne verrait le résultat de son édition qu'après
    /// avoir enregistré et rouvert le fichier.
    #[test]
    fn highlight_is_visible_in_render_before_any_save() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);
        let mut session = Session::open(&path).unwrap();

        let before = session.render_current_page(1.0).unwrap();
        let before_pixel_index = (175 * before.width as usize + 150) * 4;
        assert_eq!(
            &before.rgba[before_pixel_index..before_pixel_index + 3],
            &[255, 255, 255],
            "expected a blank area before the highlight"
        );

        session
            .add_highlight_on_current_page([100.0, 600.0, 300.0, 630.0], (1.0, 1.0, 0.0))
            .unwrap();

        let after = session.render_current_page(1.0).unwrap();
        // (150, 175) en espace pixmap (haut-gauche) tombe dans le rectangle
        // surligné [100,600]-[300,630] en espace page (bas-gauche) sur une
        // page 612x792 : y_pixmap = 792 - y_page, donc y_page in [600,630]
        // correspond à y_pixmap in [162,192].
        let after_pixel_index = (175 * after.width as usize + 150) * 4;
        // Jaune (1,1,0) à `HIGHLIGHT_FILL_ALPHA` (0.4) sur fond blanc (voir
        // `pdf_edit::EditSession::add_highlight_annotation`) : le résultat
        // n'est pas le jaune pur, mais une teinte pâle qui laisse deviner le
        // fond — exactement le but de l'opacité partielle.
        assert_eq!(
            &after.rgba[after_pixel_index..after_pixel_index + 3],
            &[255, 255, 153],
            "expected the yellow highlight to be visible right after adding it, before any save"
        );

        std::fs::remove_file(&path).ok();
    }

    /// `undo`/`redo` doivent aussi rafraîchir le rendu immédiatement.
    #[test]
    fn undo_and_redo_refresh_the_render() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);
        let mut session = Session::open(&path).unwrap();

        assert!(!session.can_undo_edit());
        session
            .add_highlight_on_current_page([100.0, 600.0, 300.0, 630.0], (1.0, 1.0, 0.0))
            .unwrap();
        assert!(session.can_undo_edit());

        let pixel_index = (175 * 612 + 150) * 4;
        let with_highlight = session.render_current_page(1.0).unwrap();
        assert_eq!(
            &with_highlight.rgba[pixel_index..pixel_index + 3],
            &[255, 255, 153]
        );

        assert!(session.undo_edit().unwrap());
        let after_undo = session.render_current_page(1.0).unwrap();
        assert_eq!(
            &after_undo.rgba[pixel_index..pixel_index + 3],
            &[255, 255, 255]
        );
        assert!(session.can_redo_edit());

        assert!(session.redo_edit().unwrap());
        let after_redo = session.render_current_page(1.0).unwrap();
        assert_eq!(
            &after_redo.rgba[pixel_index..pixel_index + 3],
            &[255, 255, 153]
        );

        std::fs::remove_file(&path).ok();
    }

    /// `save_edits_in_place` doit écrire dans le fichier ouvert par la
    /// session (pas un chemin séparé), et le résultat doit être rouvrable
    /// avec l'annotation bien présente.
    #[test]
    fn save_edits_in_place_persists_to_the_original_path() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);
        let mut session = Session::open(&path).unwrap();

        session
            .add_highlight_on_current_page([100.0, 600.0, 300.0, 630.0], (1.0, 1.0, 0.0))
            .unwrap();
        session.save_edits_in_place().unwrap();

        let reopened = pdf_core::Document::open(std::fs::read(&path).unwrap()).unwrap();
        let page = reopened.page(0).unwrap();
        let annots = page
            .dict
            .get("Annots")
            .and_then(|o| o.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        assert_eq!(annots, 1);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn insert_blank_page_at_increases_page_count_and_refreshes_render() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);
        let mut session = Session::open(&path).unwrap();

        session.insert_blank_page_at(0).unwrap();
        assert_eq!(session.page_count(), 6);
        // La nouvelle page (vierge) doit être rastérisable sans erreur.
        assert!(session.render_page(0, 1.0).is_ok());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn delete_page_at_decreases_page_count_and_undo_restores_it() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);
        let mut session = Session::open(&path).unwrap();

        session.delete_page_at(2).unwrap();
        assert_eq!(session.page_count(), 4);

        assert!(session.undo_edit().unwrap());
        assert_eq!(session.page_count(), 5);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn move_page_reorders_pages() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);
        let mut session = Session::open(&path).unwrap();

        let before = session.extract_all_text().unwrap();
        session.move_page(0, 4).unwrap();
        let after = session.extract_all_text().unwrap();
        assert_ne!(
            before, after,
            "reordering pages must change the reading order"
        );
        assert_eq!(session.page_count(), 5, "moving keeps the same page count");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn rotate_page_at_persists_after_save_and_reopen() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);
        let mut session = Session::open(&path).unwrap();

        session.rotate_page_at(0, 90).unwrap();
        session.save_edits_in_place().unwrap();

        let reopened = pdf_core::Document::open(std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(reopened.page(0).unwrap().rotate, 90);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn merge_document_from_path_appends_all_pages_of_the_source() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let base_path = write_fixture(&bytes);
        let source_bytes =
            include_bytes!("../../pdf-core/tests/fixtures/embedded_truetype_font.pdf").to_vec();
        let source_path = write_fixture(&source_bytes);
        let mut session = Session::open(&base_path).unwrap();

        session.merge_document_from_path(&source_path).unwrap();
        assert_eq!(session.page_count(), 6); // 5 pages d'origine + 1 fusionnée.
        assert!(session.render_page(5, 1.0).is_ok());

        std::fs::remove_file(&base_path).ok();
        std::fs::remove_file(&source_path).ok();
    }

    #[test]
    fn extract_pages_to_file_produces_a_standalone_document() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);
        let session = Session::open(&path).unwrap();

        let dest = std::env::temp_dir().join(format!(
            "pdf_app_test_extract_{}_{:p}.pdf",
            std::process::id(),
            &bytes
        ));
        session.extract_pages_to_file(&[1, 3], &dest).unwrap();

        let extracted = pdf_core::Document::open(std::fs::read(&dest).unwrap()).unwrap();
        assert_eq!(extracted.page_count().unwrap(), 2);

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&dest).ok();
    }

    #[test]
    fn annotations_on_current_page_lists_added_annotations_and_survives_removal() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);
        let mut session = Session::open(&path).unwrap();

        assert!(session.annotations_on_current_page().unwrap().is_empty());

        session
            .add_highlight_on_current_page([100.0, 600.0, 300.0, 630.0], (1.0, 1.0, 0.0))
            .unwrap();
        session
            .add_underline_on_current_page([100.0, 500.0, 300.0, 530.0], (1.0, 0.0, 0.0))
            .unwrap();

        let annots = session.annotations_on_current_page().unwrap();
        assert_eq!(annots.len(), 2);
        assert_eq!(annots[0].subtype, "Highlight");
        assert_eq!(annots[0].rect, [100.0, 600.0, 300.0, 630.0]);
        assert_eq!(annots[1].subtype, "Underline");

        session
            .remove_annotation_on_current_page(annots[0].index)
            .unwrap();
        let after_removal = session.annotations_on_current_page().unwrap();
        assert_eq!(after_removal.len(), 1);
        assert_eq!(after_removal[0].subtype, "Underline");

        std::fs::remove_file(&path).ok();
    }

    /// `form_fields_on_current_page` retrouve le champ du fixture (filtré
    /// via les `/Annots` de la page courante) et
    /// `set_form_field_value_on_current_page` met sa valeur à jour et
    /// rafraîchit le rendu, comme les autres méthodes d'édition
    /// `*_on_current_page`.
    #[test]
    fn form_fields_on_current_page_lists_and_fills_field() {
        let bytes = include_bytes!("../../pdf-core/tests/fixtures/acroform_textfield.pdf").to_vec();
        let path = write_fixture(&bytes);
        let mut session = Session::open(&path).unwrap();

        let fields = session.form_fields_on_current_page().unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].name, "name_field");
        assert_eq!(fields[0].value, "");

        session
            .set_form_field_value_on_current_page("name_field", "Ada Lovelace")
            .unwrap();
        let fields = session.form_fields_on_current_page().unwrap();
        assert_eq!(fields[0].value, "Ada Lovelace");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn replace_text_on_current_page_keeps_original_text_extractable() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);
        let mut session = Session::open(&path).unwrap();

        session
            .replace_text_on_current_page(
                [72.0, 715.0, 400.0, 735.0],
                "Titre remplace",
                18.0,
                (0.0, 0.0, 0.0),
                (1.0, 1.0, 1.0),
            )
            .unwrap();

        let text = session.extract_current_page_text().unwrap();
        assert!(
            text.contains("Page 1 - Hello, PDF Manager!"),
            "original text must remain extractable behind the overlay, got: {text:?}"
        );
        assert!(
            text.contains("Titre remplace"),
            "new overlay text must be extractable too, got: {text:?}"
        );

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn export_optimized_produces_a_reopenable_document_with_all_pages() {
        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let path = write_fixture(&bytes);
        let mut session = Session::open(&path).unwrap();
        session
            .add_highlight_on_current_page([100.0, 600.0, 300.0, 630.0], (1.0, 1.0, 0.0))
            .unwrap();

        let optimized = session.export_optimized().unwrap();
        let reopened = pdf_core::Document::open(optimized).unwrap();
        assert_eq!(reopened.page_count().unwrap(), 5);
        let page = reopened.page(0).unwrap();
        let annots = page
            .dict
            .get("Annots")
            .and_then(|o| o.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        assert_eq!(annots, 1, "the pending highlight must survive optimization");

        std::fs::remove_file(&path).ok();
    }
}
