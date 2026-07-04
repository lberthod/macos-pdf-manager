//! Prototype de viewer PDF (egui) — architecture.md §8.1 : "commencer le
//! prototype en egui pour valider les flux, migrer le chrome vers natif
//! ensuite". Ce binaire parle à `pdf-app::Session` pour l'état de session
//! (document ouvert, page courante, rendu) — voir STATUS.md, ce n'est plus
//! un raccourci direct vers `pdf-core`/`pdf-render`.
//!
//! Fonctionnalités : ouverture de fichier (dialogue natif via `rfd`),
//! navigation page suivante/précédente, zoom par boutons **et** par
//! molette+Ctrl/pincement trackpad (`egui::InputState::zoom_delta`,
//! re-rasterisation à chaque cran, pas un agrandissement d'image),
//! recherche texte (`Session::find_pages_containing`) qui saute
//! d'occurrence en occurrence page par page **avec surlignage** des
//! résultats sur la page affichée (`Session::find_matches_on_current_page`),
//! panneau de miniatures cliquables et panneau de signets (`/Outlines`,
//! `Session::outline`) pour naviguer directement à une page, un mode
//! **défilement continu** (`egui::ScrollArea::show_rows`, virtualisé : seules
//! les pages proches de la zone visible sont rastérisées) qui affiche
//! toutes les pages empilées verticalement au lieu d'une à la fois, et la
//! **sélection de texte à la souris** (glisser sur la page en mode page
//! unique, via `Session::char_index_at_on_current_page`/
//! `selection_on_current_page`) avec copie dans le presse-papiers (bouton
//! ou ⌘C).
//!
//! Non géré (voir STATUS.md) : onglets/multi-documents, annotations,
//! raccourcis clavier au-delà des boutons/⌘C, sélection de texte en mode
//! défilement continu (page unique seulement). Limitation du défilement
//! continu : la hauteur de ligne est calculée une fois sur la page 0
//! (documents à pages de tailles hétérogènes mal gérés).

mod native_menu;

use eframe::egui;
use native_menu::{MenuCommand, NativeMenu};
use pdf_app::Session;
use std::collections::HashMap;
use std::path::PathBuf;

const ZOOM_MIN: f32 = 0.25;
const ZOOM_MAX: f32 = 4.0;
/// Échelle de rendu des miniatures (page 612pt de large -> ~92px).
const THUMBNAIL_SCALE: f64 = 0.15;
/// Jaune translucide pour le surlignage des résultats de recherche.
const HIGHLIGHT_COLOR: egui::Color32 = egui::Color32::from_rgba_premultiplied(90, 85, 10, 90);
/// Bleu translucide pour la sélection de texte à la souris.
const SELECTION_COLOR: egui::Color32 = egui::Color32::from_rgba_premultiplied(20, 60, 110, 90);

fn main() -> eframe::Result<()> {
    // Backend `wgpu` plutôt que le `glow` par défaut d'eframe : condition
    // nécessaire pour partager le `Device`/`Queue` d'eframe avec
    // `pdf-render-gpu` (voir `ViewerApp::new` et
    // `pdf_render_gpu::GpuRenderer::from_shared`) — sans quoi ce backend
    // devrait renégocier son propre device à chaque page (voir la doc de
    // module de `pdf-render-gpu`, le problème que cette intégration résout).
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "PDF Manager (prototype)",
        options,
        Box::new(|cc| {
            let gpu = cc.wgpu_render_state.as_ref().map(|rs| {
                pdf_render_gpu::GpuRenderer::from_shared(rs.device.clone(), rs.queue.clone())
            });
            // La barre de menus native n'est **pas** installée ici : ce
            // callback tourne avant que la boucle d'événements `winit`
            // démarre réellement, et `winit` installe son propre menu par
            // défaut (juste "Quitter") au démarrage de cette boucle — après
            // ce callback. L'installer ici risquait donc de se faire
            // silencieusement écraser. `ViewerApp::update` l'installe plutôt
            // à sa toute première frame, garantie de tourner une fois la
            // boucle `winit`/AppKit pleinement démarrée.
            Ok(Box::new(ViewerApp::new(std::env::args().nth(1), gpu)))
        }),
    )
}

struct ViewerApp {
    session: Option<Session>,
    zoom: f32,
    texture: Option<egui::TextureHandle>,
    /// (page affichée, zoom affiché) par la texture courante — sert à
    /// détecter qu'un nouveau rendu est nécessaire sans re-rasteriser à
    /// chaque frame.
    texture_state: Option<(usize, u32)>,
    error: Option<String>,
    search_query: String,
    /// Index des pages (0-based) contenant `search_query`, dans l'ordre du
    /// document ; `None` tant qu'aucune recherche n'a été lancée.
    search_results: Option<Vec<usize>>,
    /// Position courante dans `search_results` pour "occurrence suivante".
    search_cursor: usize,
    /// Requête effectivement recherchée (fige la valeur de `search_query`
    /// au moment de `run_search`, pour que le surlignage ne change pas
    /// pendant que l'utilisateur retape dans le champ).
    highlighted_query: String,
    /// (page affichée, requête surlignée) pour laquelle `highlight_rects`
    /// est à jour — évite de refaire la recherche de position à chaque frame.
    highlight_state: Option<(usize, String)>,
    highlight_rects: Vec<pdf_app::MatchRect>,
    show_thumbnails: bool,
    thumbnails: HashMap<usize, egui::TextureHandle>,
    show_outline: bool,
    continuous_scroll: bool,
    /// Textures du mode défilement continu, indépendantes de `texture`
    /// (mode page unique) : une par page déjà rastérisée à l'échelle
    /// courante. Vidées quand le zoom change (voir `page_textures_zoom_key`).
    page_textures: HashMap<usize, egui::TextureHandle>,
    page_textures_zoom_key: Option<u32>,
    /// Page à faire défiler jusqu'à l'écran au prochain rendu du mode
    /// continu (navigation depuis la recherche/miniatures/signets/boutons) ;
    /// consommé (mis à `None`) une fois appliqué pour ne pas entraver le
    /// défilement manuel de l'utilisateur ensuite.
    scroll_to_page: Option<usize>,
    /// Page sur laquelle porte la sélection de texte courante — sert à
    /// l'invalider si l'utilisateur change de page sans avoir relâché de
    /// sélection (les indices de caractères n'ont de sens que par page).
    selection_page: Option<usize>,
    /// Indice de caractère où le glissement de sélection a commencé.
    selection_anchor: Option<usize>,
    /// Indice de caractère sous le pointeur pendant/après le glissement.
    selection_cursor: Option<usize>,
    selection_rects: Vec<pdf_app::MatchRect>,
    selection_text: String,
    /// `Device`/`Queue` partagés avec le renderer `wgpu` d'eframe (voir
    /// `main`) — `None` si le backend `glow` a été sélectionné ou si aucun
    /// adaptateur `wgpu` n'était disponible au démarrage. Cloné dans chaque
    /// `Session` ouverte (voir `open_path`) : `GpuRenderer` ne fait que
    /// partager des `Arc`, un clone est donc bon marché.
    gpu: Option<pdf_render_gpu::GpuRenderer>,
    /// Barre de menus native macOS (Sprint 11-12, sprint.md) — `None` sur
    /// les plateformes non macOS ou si l'installation a échoué, auquel cas
    /// seuls la barre d'outils `egui` et le glisser-déposer restent
    /// utilisables pour ouvrir un fichier.
    native_menu: Option<NativeMenu>,
}

impl ViewerApp {
    fn new(initial_path: Option<String>, gpu: Option<pdf_render_gpu::GpuRenderer>) -> Self {
        let mut app = Self {
            session: None,
            gpu,
            native_menu: None,
            zoom: 1.0,
            texture: None,
            texture_state: None,
            error: None,
            search_query: String::new(),
            search_results: None,
            search_cursor: 0,
            highlighted_query: String::new(),
            highlight_state: None,
            highlight_rects: Vec::new(),
            show_thumbnails: false,
            thumbnails: HashMap::new(),
            show_outline: false,
            continuous_scroll: false,
            page_textures: HashMap::new(),
            page_textures_zoom_key: None,
            scroll_to_page: None,
            selection_page: None,
            selection_anchor: None,
            selection_cursor: None,
            selection_rects: Vec::new(),
            selection_text: String::new(),
        };
        if let Some(p) = initial_path {
            app.open_path(PathBuf::from(p));
        }
        app
    }

    fn open_path(&mut self, path: PathBuf) {
        self.error = None;
        self.texture = None;
        self.texture_state = None;
        self.search_results = None;
        self.search_cursor = 0;
        self.highlighted_query.clear();
        self.highlight_state = None;
        self.highlight_rects.clear();
        self.thumbnails.clear();
        self.page_textures.clear();
        self.page_textures_zoom_key = None;
        self.scroll_to_page = None;
        self.selection_page = None;
        self.selection_anchor = None;
        self.selection_cursor = None;
        self.selection_rects.clear();
        self.selection_text.clear();

        match Session::open(path) {
            Ok(mut session) => {
                if let Some(gpu) = &self.gpu {
                    session.set_gpu_renderer(gpu.clone());
                }
                self.session = Some(session);
            }
            Err(e) => {
                self.error = Some(format!("Impossible d'ouvrir le fichier : {e}"));
                self.session = None;
            }
        }
    }

    /// Ouvre le sélecteur de fichier natif (`rfd`, `NSOpenPanel` sous le
    /// capot) et charge le fichier choisi — partagé entre le bouton de la
    /// barre d'outils et le menu natif "Fichier > Ouvrir…".
    fn open_file_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .pick_file()
        {
            self.open_path(path);
        }
    }

    /// "Fichier > Exporter une copie…" : copie le fichier actuellement
    /// ouvert vers un nouvel emplacement choisi via `NSSavePanel` (`rfd`).
    /// Il n'y a pas de sauvegarde "en place" à proprement parler tant que
    /// `pdf-edit` (édition, undo/redo) n'existe pas — voir `native_menu.rs`
    /// pour pourquoi `⌘Z`/`⌘⇧Z` ne sont volontairement pas câblés encore.
    fn export_copy_as(&mut self) {
        let Some(session) = &self.session else {
            self.error = Some("Aucun document ouvert à exporter.".to_string());
            return;
        };
        let source = session.path().to_path_buf();
        let default_name = source
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "document.pdf".to_string());
        if let Some(dest) = rfd::FileDialog::new()
            .add_filter("PDF", &["pdf"])
            .set_file_name(&default_name)
            .save_file()
        {
            if let Err(e) = std::fs::copy(&source, &dest) {
                self.error = Some(format!("Échec de l'export : {e}"));
            }
        }
    }

    /// Lance une recherche plein document et saute directement à la première
    /// page trouvée (s'il y en a une).
    fn run_search(&mut self) {
        let Some(session) = &mut self.session else {
            return;
        };
        match session.find_pages_containing(&self.search_query) {
            Ok(results) => {
                self.search_cursor = 0;
                if let Some(&first) = results.first() {
                    session.goto_page(first);
                    self.scroll_to_page = Some(first);
                }
                self.search_results = Some(results);
                self.highlighted_query = self.search_query.clone();
                self.highlight_state = None; // force le recalcul du surlignage.
            }
            Err(e) => self.error = Some(format!("Erreur de recherche : {e}")),
        }
    }

    fn set_zoom(&mut self, zoom: f32) {
        self.zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
    }

    /// Saute à l'occurrence suivante/précédente (cyclique) dans
    /// `search_results`, sans relancer la recherche.
    fn jump_to_match(&mut self, delta: isize) {
        let Some(results) = &self.search_results else {
            return;
        };
        if results.is_empty() {
            return;
        }
        let len = results.len() as isize;
        let next = (self.search_cursor as isize + delta).rem_euclid(len) as usize;
        self.search_cursor = next;
        let target = results[next];
        if let Some(session) = &mut self.session {
            session.goto_page(target);
        }
        self.scroll_to_page = Some(target);
    }

    /// Re-rastérise la page courante si la page ou le zoom affiché a changé
    /// depuis la dernière frame ; sinon réutilise la texture GPU existante.
    fn ensure_texture(&mut self, ctx: &egui::Context) {
        let Some(session) = &self.session else {
            return;
        };
        if session.page_count() == 0 {
            return;
        }

        // Quantifie le zoom pour éviter de re-rasteriser à chaque frame à
        // cause du bruit en virgule flottante des sliders.
        let zoom_key = (self.zoom * 1000.0).round() as u32;
        let page_index = session.page_index();
        if self.texture_state == Some((page_index, zoom_key)) {
            return; // déjà à jour.
        }

        match session.render_current_page(self.zoom as f64) {
            Ok(rendered) => {
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [rendered.width as usize, rendered.height as usize],
                    &rendered.rgba,
                );
                self.texture = Some(ctx.load_texture("page", image, egui::TextureOptions::LINEAR));
                self.texture_state = Some((page_index, zoom_key));
                self.error = None;
            }
            Err(e) => {
                self.error = Some(format!("Erreur de rendu page {}: {e}", page_index));
                self.texture = None;
            }
        }
    }

    /// Recalcule `highlight_rects` si la page affichée ou la requête
    /// surlignée a changé depuis le dernier appel.
    fn ensure_highlights(&mut self) {
        let Some(session) = &self.session else {
            self.highlight_rects.clear();
            return;
        };
        if self.highlighted_query.is_empty() {
            self.highlight_rects.clear();
            self.highlight_state = None;
            return;
        }

        let key = (session.page_index(), self.highlighted_query.clone());
        if self.highlight_state.as_ref() == Some(&key) {
            return; // déjà à jour.
        }

        match session.find_matches_on_current_page(&self.highlighted_query) {
            Ok(rects) => self.highlight_rects = rects,
            Err(_) => self.highlight_rects.clear(),
        }
        self.highlight_state = Some(key);
    }

    /// Charge (et met en cache) la texture de la miniature de `index`, si ce
    /// n'est pas déjà fait.
    fn ensure_thumbnail(&mut self, index: usize, ctx: &egui::Context) {
        if self.thumbnails.contains_key(&index) {
            return;
        }
        let Some(session) = &self.session else {
            return;
        };
        if let Ok(rendered) = session.render_page(index, THUMBNAIL_SCALE) {
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [rendered.width as usize, rendered.height as usize],
                &rendered.rgba,
            );
            let texture = ctx.load_texture(
                format!("thumb-{index}"),
                image,
                egui::TextureOptions::LINEAR,
            );
            self.thumbnails.insert(index, texture);
        }
    }

    /// Dessine le panneau de miniatures et retourne l'index de page cliqué,
    /// le cas échéant (la navigation elle-même est faite par l'appelant pour
    /// éviter d'emprunter `self.session` en même temps que `self.thumbnails`).
    fn show_thumbnail_panel(&mut self, ctx: &egui::Context) -> Option<usize> {
        let page_count = self.session.as_ref()?.page_count();
        let current = self.session.as_ref()?.page_index();
        let mut clicked = None;

        egui::SidePanel::left("thumbnails")
            .resizable(true)
            .default_width(140.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for index in 0..page_count {
                        self.ensure_thumbnail(index, ctx);
                        let Some(texture) = self.thumbnails.get(&index) else {
                            continue;
                        };
                        ui.vertical_centered(|ui| {
                            let response =
                                ui.add(egui::ImageButton::new(texture).selected(index == current));
                            if response.clicked() {
                                clicked = Some(index);
                            }
                            ui.label(format!("{}", index + 1));
                        });
                    }
                });
            });

        clicked
    }

    /// Met à jour la sélection de texte à partir du glissement de souris sur
    /// `response` (l'image de la page), et recalcule le texte/les
    /// rectangles sélectionnés en conséquence. Invalide la sélection
    /// précédente si la page affichée a changé entre-temps.
    fn handle_text_selection(
        &mut self,
        response: &egui::Response,
        media_box: [f64; 4],
        scale: f64,
    ) {
        let Some(page_index) = self.session.as_ref().map(|s| s.page_index()) else {
            return;
        };
        if self.selection_page != Some(page_index) {
            self.selection_anchor = None;
            self.selection_cursor = None;
            self.selection_rects.clear();
            self.selection_text.clear();
            self.selection_page = Some(page_index);
        }

        if response.drag_started() {
            if let Some(pos) = response.interact_pointer_pos() {
                let point = screen_to_page(pos, response.rect, media_box, scale);
                self.selection_anchor = self
                    .session
                    .as_ref()
                    .and_then(|s| s.char_index_at_on_current_page(point).ok().flatten());
                self.selection_cursor = self.selection_anchor;
            }
        } else if response.dragged() {
            if let Some(pos) = response.interact_pointer_pos() {
                let point = screen_to_page(pos, response.rect, media_box, scale);
                self.selection_cursor = self
                    .session
                    .as_ref()
                    .and_then(|s| s.char_index_at_on_current_page(point).ok().flatten());
            }
        } else if response.clicked() {
            // Simple clic (pas de glissement) : efface la sélection.
            self.selection_anchor = None;
            self.selection_cursor = None;
        }

        let range = match (self.selection_anchor, self.selection_cursor) {
            (Some(a), Some(b)) if a != b => Some(a.min(b)..a.max(b) + 1),
            _ => None,
        };
        match range {
            Some(range) => match self
                .session
                .as_ref()
                .map(|s| s.selection_on_current_page(range))
            {
                Some(Ok((text, rects))) => {
                    self.selection_text = text;
                    self.selection_rects = rects;
                }
                _ => {
                    self.selection_text.clear();
                    self.selection_rects.clear();
                }
            },
            None => {
                self.selection_text.clear();
                self.selection_rects.clear();
            }
        }
    }

    /// Dessine le panneau de signets (table des matières) et retourne
    /// l'index de page cliqué, le cas échéant. `None` (pas seulement un
    /// panneau vide) si le document n'a pas de `/Outlines` — l'appelant
    /// n'affiche alors pas le panneau du tout.
    fn show_outline_panel(&mut self, ctx: &egui::Context) -> Option<usize> {
        let outline = self.session.as_ref()?.outline().ok()?;
        if outline.is_empty() {
            return None;
        }

        let mut clicked = None;
        egui::SidePanel::left("outline")
            .resizable(true)
            .default_width(180.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    render_outline_items(ui, &outline, 0, &mut clicked);
                });
            });
        clicked
    }
}

/// Affiche récursivement une table des matières, avec une indentation par
/// niveau de profondeur. Les entrées sans page résolue (voir
/// `pdf_app::OutlineItem::page_index`, ex. destination nommée non gérée)
/// restent affichées mais ne naviguent nulle part au clic.
fn render_outline_items(
    ui: &mut egui::Ui,
    items: &[pdf_app::OutlineItem],
    depth: usize,
    clicked: &mut Option<usize>,
) {
    for item in items {
        ui.horizontal(|ui| {
            ui.add_space(depth as f32 * 12.0);
            if ui.selectable_label(false, &item.title).clicked() {
                if let Some(page) = item.page_index {
                    *clicked = Some(page);
                }
            }
        });
        render_outline_items(ui, &item.children, depth + 1, clicked);
    }
}

impl ViewerApp {
    /// Affiche toutes les pages du document empilées verticalement dans une
    /// seule zone de défilement. Virtualisé via `ScrollArea::show_rows` :
    /// seules les pages dont la ligne tombe dans (ou près de) la zone
    /// visible sont rastérisées/chargées en texture, ce qui reste praticable
    /// sur un document de plusieurs centaines de pages.
    fn show_continuous_scroll(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let zoom_key = (self.zoom * 1000.0).round() as u32;
        if self.page_textures_zoom_key != Some(zoom_key) {
            self.page_textures.clear();
            self.page_textures_zoom_key = Some(zoom_key);
        }

        let zoom = self.zoom as f64;
        let Some(session) = &self.session else {
            return;
        };
        let page_count = session.page_count();
        if page_count == 0 {
            return;
        }
        // Hauteur de ligne uniforme dérivée de la page 0 (voir limitation
        // en tête de fichier : documents à pages de tailles hétérogènes).
        let Ok(media_box) = session.page_media_box(0) else {
            return;
        };
        let row_height = ((media_box[3] - media_box[1]) * zoom) as f32 + 8.0;

        let mut scroll_area = egui::ScrollArea::vertical();
        if let Some(target) = self.scroll_to_page.take() {
            scroll_area = scroll_area.vertical_scroll_offset(target as f32 * row_height);
        }

        let page_textures = &mut self.page_textures;
        scroll_area.show_rows(ui, row_height, page_count, |ui, row_range| {
            for index in row_range {
                if let std::collections::hash_map::Entry::Vacant(entry) = page_textures.entry(index)
                {
                    if let Ok(rendered) = session.render_page(index, zoom) {
                        let image = egui::ColorImage::from_rgba_unmultiplied(
                            [rendered.width as usize, rendered.height as usize],
                            &rendered.rgba,
                        );
                        let texture = ctx.load_texture(
                            format!("page-{index}"),
                            image,
                            egui::TextureOptions::LINEAR,
                        );
                        entry.insert(texture);
                    }
                }
                if let Some(texture) = page_textures.get(&index) {
                    ui.vertical_centered(|ui| {
                        ui.image(texture);
                    });
                }
                ui.add_space(8.0);
            }
        });
    }
}

impl eframe::App for ViewerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Installée ici (première frame seulement) plutôt que dans le
        // callback de création de `main` : à ce stade, la boucle
        // d'événements `winit`/AppKit a fini son propre démarrage (qui
        // installe un menu par défaut), donc la nôtre ne se fait plus
        // écraser — voir la doc de `main` et `NativeMenu::install`.
        if self.native_menu.is_none() {
            if let Some(mtm) = objc2::MainThreadMarker::new() {
                self.native_menu = Some(NativeMenu::install(mtm));
            }
        }

        // Commandes émises par la barre de menus native depuis la dernière
        // frame (voir `native_menu.rs`) — traitées ici plutôt que dans le
        // callback Objective-C lui-même, pour ne coupler `MenuTarget` qu'à
        // un canal MPSC plutôt qu'à l'état `egui`.
        if let Some(menu) = &self.native_menu {
            for cmd in menu.drain_commands() {
                match cmd {
                    MenuCommand::OpenDocument => self.open_file_dialog(),
                    MenuCommand::ExportCopyAs => self.export_copy_as(),
                    MenuCommand::ToggleDarkMode => {
                        if let Some(mtm) = objc2::MainThreadMarker::new() {
                            let dark = native_menu::toggle_dark_mode(mtm);
                            ctx.set_visuals(if dark {
                                egui::Visuals::dark()
                            } else {
                                egui::Visuals::light()
                            });
                        }
                    }
                }
            }
        }

        // Glisser-déposer un fichier PDF sur la fenêtre (`egui` expose déjà
        // les fichiers déposés via l'événement natif `NSWindow`/`winit` sans
        // code Objective-C supplémentaire).
        let dropped_path = ctx.input(|i| i.raw.dropped_files.first().and_then(|f| f.path.clone()));
        if let Some(path) = dropped_path {
            self.open_path(path);
        }

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Ouvrir…").clicked() {
                    self.open_file_dialog();
                }

                ui.separator();

                let (page_index, page_count) = self
                    .session
                    .as_ref()
                    .map(|s| (s.page_index(), s.page_count()))
                    .unwrap_or((0, 0));
                let has_doc = self.session.is_some();

                ui.add_enabled_ui(has_doc, |ui| {
                    ui.toggle_value(&mut self.show_thumbnails, "🖼 Miniatures");
                    ui.toggle_value(&mut self.show_outline, "📑 Signets");
                    ui.toggle_value(&mut self.continuous_scroll, "📜 Continu");
                });

                ui.separator();

                ui.add_enabled_ui(has_doc && page_index > 0, |ui| {
                    if ui.button("◀ Précédente").clicked() {
                        if let Some(session) = &mut self.session {
                            session.prev_page();
                            self.scroll_to_page = Some(session.page_index());
                        }
                    }
                });
                ui.label(if has_doc {
                    format!("Page {} / {}", page_index + 1, page_count.max(1))
                } else {
                    "Aucun document".to_string()
                });
                ui.add_enabled_ui(has_doc && page_index + 1 < page_count, |ui| {
                    if ui.button("Suivante ▶").clicked() {
                        if let Some(session) = &mut self.session {
                            session.next_page();
                            self.scroll_to_page = Some(session.page_index());
                        }
                    }
                });

                ui.separator();

                ui.add_enabled_ui(has_doc, |ui| {
                    if ui.button("－").clicked() {
                        self.set_zoom(self.zoom - 0.25);
                    }
                    ui.label(format!("{:.0}%", self.zoom * 100.0));
                    if ui.button("＋").clicked() {
                        self.set_zoom(self.zoom + 0.25);
                    }
                    if ui.button("Réinitialiser").clicked() {
                        self.set_zoom(1.0);
                    }
                });

                if !self.selection_text.is_empty() {
                    ui.separator();
                    if ui.button("📋 Copier").clicked() {
                        ui.output_mut(|o| o.copied_text = self.selection_text.clone());
                    }
                }

                if let Some(session) = &self.session {
                    ui.separator();
                    ui.label(
                        session
                            .path()
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy(),
                    );
                }
            });

            ui.horizontal(|ui| {
                let has_doc = self.session.is_some();
                ui.add_enabled_ui(has_doc, |ui| {
                    let response = ui.text_edit_singleline(&mut self.search_query);
                    let submitted =
                        response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button("🔍 Rechercher").clicked() || submitted {
                        self.run_search();
                    }
                });

                if let Some(results) = &self.search_results {
                    if results.is_empty() {
                        ui.label("Aucun résultat");
                    } else {
                        ui.label(format!(
                            "{}/{} pages",
                            self.search_cursor + 1,
                            results.len()
                        ));
                        if ui.button("◀ Précédent").clicked() {
                            self.jump_to_match(-1);
                        }
                        if ui.button("Suivant ▶").clicked() {
                            self.jump_to_match(1);
                        }
                    }
                }
            });
        });

        if self.show_thumbnails {
            if let Some(clicked) = self.show_thumbnail_panel(ctx) {
                if let Some(session) = &mut self.session {
                    session.goto_page(clicked);
                }
                self.scroll_to_page = Some(clicked);
            }
        }

        if self.show_outline {
            if let Some(clicked) = self.show_outline_panel(ctx) {
                if let Some(session) = &mut self.session {
                    session.goto_page(clicked);
                }
                self.scroll_to_page = Some(clicked);
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(err) = &self.error {
                ui.colored_label(egui::Color32::RED, err);
                return;
            }
            if self.session.is_none() {
                ui.centered_and_justified(|ui| {
                    ui.label("Ouvrez un fichier PDF pour commencer.");
                });
                return;
            }

            // Molette+Ctrl ou pincement trackpad : `zoom_delta` vaut 1.0 en
            // l'absence de tel geste (simple scroll de la molette, laissé au
            // `ScrollArea` ci-dessous pour le défilement).
            let zoom_delta = ctx.input(|i| i.zoom_delta());
            if zoom_delta != 1.0 {
                self.set_zoom(self.zoom * zoom_delta);
            }

            // ⌘C/Ctrl+C : copie la sélection de texte courante, si non vide
            // (egui traduit déjà le raccourci plateforme en `Event::Copy`).
            if !self.selection_text.is_empty()
                && ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)))
            {
                let text = self.selection_text.clone();
                ctx.output_mut(|o| o.copied_text = text);
            }

            if self.continuous_scroll {
                self.show_continuous_scroll(ui, ctx);
                return;
            }

            self.ensure_texture(ctx);
            self.ensure_highlights();

            // Clonée (bon marché : `TextureHandle` est un handle partagé)
            // pour ne pas garder de prêt sur `self.texture` pendant qu'on
            // met à jour `self.selection_*` plus bas dans la même closure.
            if let Some(texture) = self.texture.clone() {
                let media_box = self
                    .session
                    .as_ref()
                    .and_then(|s| s.current_page_media_box().ok());
                let scale = self.zoom as f64;

                egui::ScrollArea::both().show(ui, |ui| {
                    let response =
                        ui.add(egui::Image::new(&texture).sense(egui::Sense::click_and_drag()));

                    if let Some(media_box) = media_box {
                        if !self.highlight_rects.is_empty() {
                            draw_highlights(
                                ui,
                                &response,
                                &self.highlight_rects,
                                media_box,
                                scale,
                                HIGHLIGHT_COLOR,
                            );
                        }

                        self.handle_text_selection(&response, media_box, scale);
                        if !self.selection_rects.is_empty() {
                            draw_highlights(
                                ui,
                                &response,
                                &self.selection_rects,
                                media_box,
                                scale,
                                SELECTION_COLOR,
                            );
                        }
                    }
                });
            }
        });
    }
}

/// Dessine les rectangles de surlignage (espace page PDF, origine
/// bas-gauche) par-dessus l'image de la page déjà affichée à `image_response`,
/// en reproduisant la même transformation page->pixmap que `pdf-render`
/// (voir `pdf_render::page_flip_matrix`) : c'est le prix de garder `pdf-ui`
/// indépendant du backend de rendu — cette conversion devra suivre si le
/// calcul de transformation change côté rendu.
fn draw_highlights(
    ui: &egui::Ui,
    image_response: &egui::Response,
    rects: &[pdf_app::MatchRect],
    media_box: [f64; 4],
    scale: f64,
    color: egui::Color32,
) {
    let origin_x = media_box[0];
    let origin_top = media_box[3];
    let painter = ui.painter();
    for rect in rects {
        let min = egui::pos2(
            image_response.rect.min.x + ((rect.x0 - origin_x) * scale) as f32,
            image_response.rect.min.y + ((origin_top - rect.y1) * scale) as f32,
        );
        let max = egui::pos2(
            image_response.rect.min.x + ((rect.x1 - origin_x) * scale) as f32,
            image_response.rect.min.y + ((origin_top - rect.y0) * scale) as f32,
        );
        painter.rect_filled(egui::Rect::from_min_max(min, max), 0.0, color);
    }
}

/// Convertit une position écran (dans le repère de `image_rect`, le
/// rectangle occupé par l'image de la page) en point d'espace page PDF —
/// inverse de la transformation appliquée par `draw_highlights`.
fn screen_to_page(
    pos: egui::Pos2,
    image_rect: egui::Rect,
    media_box: [f64; 4],
    scale: f64,
) -> (f64, f64) {
    let origin_x = media_box[0];
    let origin_top = media_box[3];
    let page_x = origin_x + (pos.x - image_rect.min.x) as f64 / scale;
    let page_y = origin_top - (pos.y - image_rect.min.y) as f64 / scale;
    (page_x, page_y)
}
