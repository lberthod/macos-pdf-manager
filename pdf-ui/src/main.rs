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
//! `Session::outline`) pour naviguer directement à une page.
//!
//! Non géré (voir STATUS.md) : onglets/multi-documents, annotations,
//! sélection de texte à la souris, scroll continu entre pages (une page à
//! la fois), raccourcis clavier au-delà des boutons.

use eframe::egui;
use pdf_app::Session;
use std::collections::HashMap;
use std::path::PathBuf;

const ZOOM_MIN: f32 = 0.25;
const ZOOM_MAX: f32 = 4.0;
/// Échelle de rendu des miniatures (page 612pt de large -> ~92px).
const THUMBNAIL_SCALE: f64 = 0.15;
/// Jaune translucide pour le surlignage des résultats de recherche.
const HIGHLIGHT_COLOR: egui::Color32 = egui::Color32::from_rgba_premultiplied(90, 85, 10, 90);

fn main() -> eframe::Result<()> {
    eframe::run_native(
        "PDF Manager (prototype)",
        eframe::NativeOptions::default(),
        Box::new(|_cc| Ok(Box::new(ViewerApp::new(std::env::args().nth(1))))),
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
}

impl ViewerApp {
    fn new(initial_path: Option<String>) -> Self {
        let mut app = Self {
            session: None,
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

        match Session::open(path) {
            Ok(session) => self.session = Some(session),
            Err(e) => {
                self.error = Some(format!("Impossible d'ouvrir le fichier : {e}"));
                self.session = None;
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
        if let Some(session) = &mut self.session {
            session.goto_page(results[next]);
        }
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

impl eframe::App for ViewerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Ouvrir…").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("PDF", &["pdf"])
                        .pick_file()
                    {
                        self.open_path(path);
                    }
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
                });

                ui.separator();

                ui.add_enabled_ui(has_doc && page_index > 0, |ui| {
                    if ui.button("◀ Précédente").clicked() {
                        if let Some(session) = &mut self.session {
                            session.prev_page();
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
            }
        }

        if self.show_outline {
            if let Some(clicked) = self.show_outline_panel(ctx) {
                if let Some(session) = &mut self.session {
                    session.goto_page(clicked);
                }
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

            self.ensure_texture(ctx);
            self.ensure_highlights();

            if let Some(texture) = &self.texture {
                egui::ScrollArea::both().show(ui, |ui| {
                    let response = ui.image(texture);

                    if !self.highlight_rects.is_empty() {
                        if let Some(session) = &self.session {
                            if let Ok(media_box) = session.current_page_media_box() {
                                draw_highlights(
                                    ui,
                                    &response,
                                    &self.highlight_rects,
                                    media_box,
                                    self.zoom as f64,
                                );
                            }
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
        painter.rect_filled(egui::Rect::from_min_max(min, max), 0.0, HIGHLIGHT_COLOR);
    }
}
