//! État de session applicative : document ouvert, page courante, rendu
//! agnostique du backend de rasterisation — orchestration entre `pdf-core`
//! (parsing/interprétation) et `pdf-render` (rasterisation), pour que le
//! futur chrome natif macOS (Sprint 11-12) et `pdf-ui` n'aient plus à parler
//! directement à ces deux crates (voir sprint.md, STATUS.md §5).
//!
//! Ne porte pas encore d'historique d'édition (`EditOp`/undo-redo — voir
//! sprint.md Sprint 13-14) : uniquement l'état de *lecture* pour l'instant.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use pdf_core::Document;

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
}

impl Session {
    /// Ouvre un fichier PDF depuis le disque.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        let doc = Document::open(bytes).map_err(|e| e.to_string())?;
        let page_count = doc.page_count().map_err(|e| e.to_string())?;
        Ok(Self {
            doc,
            path,
            page_count,
            page_index: 0,
            text_cache: RefCell::new(vec![None; page_count]),
            render_cache: RefCell::new(Vec::new()),
            outline_cache: RefCell::new(None),
        })
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
        let pixmap = pdf_render::render_page_rotated(&display, page.media_box, page.rotate, scale)
            .ok_or_else(|| "render target allocation failed".to_string())?;
        let rendered = Rc::new(RenderedPage {
            width: pixmap.width(),
            height: pixmap.height(),
            rgba: pixmap.data().to_vec(),
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

    /// Recherche `query` (insensible à la casse) dans le texte de chaque
    /// page et retourne les index de page où au moins une occurrence a été
    /// trouvée. Le texte de chaque page n'est extrait qu'une fois (voir
    /// `text_cache`) : une deuxième recherche sur le même document ne
    /// ré-interprète aucun flux de contenu.
    pub fn find_pages_containing(&self, query: &str) -> Result<Vec<usize>, String> {
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let query = query.to_lowercase();
        let mut matches = Vec::new();
        for index in 0..self.page_count {
            if self
                .cached_page_text(index)?
                .text()
                .to_lowercase()
                .contains(&query)
            {
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
        let display =
            pdf_core::interp::Interpreter::run_page(&self.doc, page.resources.clone(), &content)
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
}
