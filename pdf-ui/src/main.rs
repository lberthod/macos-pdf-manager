//! Prototype de viewer PDF (egui) — architecture.md §8.1 : "commencer le
//! prototype en egui pour valider les flux, migrer le chrome vers natif
//! ensuite". Ce binaire parle directement à `pdf-core`/`pdf-render` plutôt
//! qu'à `pdf-app` (stub vide pour l'instant) : c'est un raccourci assumé
//! pour ce prototype, à revoir quand `pdf-app` portera l'état de session.
//!
//! Fonctionnalités : ouverture de fichier (dialogue natif via `rfd`),
//! navigation page suivante/précédente, zoom (re-rasterisation, pas un
//! agrandissement d'image).
//!
//! Non géré (voir STATUS.md) : onglets/multi-documents, annotations,
//! recherche de texte, miniatures, raccourcis clavier au-delà des boutons.

use eframe::egui;
use pdf_core::Document;
use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    eframe::run_native(
        "PDF Manager (prototype)",
        eframe::NativeOptions::default(),
        Box::new(|_cc| Ok(Box::new(ViewerApp::new(std::env::args().nth(1))))),
    )
}

struct ViewerApp {
    doc: Option<Document>,
    path: Option<PathBuf>,
    page_index: usize,
    page_count: usize,
    zoom: f32,
    texture: Option<egui::TextureHandle>,
    /// (page affichée, zoom affiché) par la texture courante — sert à
    /// détecter qu'un nouveau rendu est nécessaire sans re-rasteriser à
    /// chaque frame.
    texture_state: Option<(usize, u32)>,
    error: Option<String>,
}

impl ViewerApp {
    fn new(initial_path: Option<String>) -> Self {
        let mut app = Self {
            doc: None,
            path: None,
            page_index: 0,
            page_count: 0,
            zoom: 1.0,
            texture: None,
            texture_state: None,
            error: None,
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
        self.page_index = 0;

        match std::fs::read(&path)
            .map_err(|e| e.to_string())
            .and_then(|bytes| Document::open(bytes).map_err(|e| e.to_string()))
        {
            Ok(doc) => {
                self.page_count = doc.page_count().unwrap_or(0);
                self.doc = Some(doc);
                self.path = Some(path);
            }
            Err(e) => {
                self.error = Some(format!("Impossible d'ouvrir le fichier : {e}"));
                self.doc = None;
                self.path = None;
                self.page_count = 0;
            }
        }
    }

    /// Redonne le texte d'erreur affiché s'il y en a un pour la page
    /// courante (rendu séparé de l'ouverture, car une page individuelle
    /// peut échouer même si le document s'est ouvert correctement).
    fn ensure_texture(&mut self, ctx: &egui::Context) {
        let Some(doc) = &self.doc else { return };
        if self.page_count == 0 {
            return;
        }

        // Quantifie le zoom pour éviter de re-rasteriser à chaque frame à
        // cause du bruit en virgule flottante des sliders.
        let zoom_key = (self.zoom * 1000.0).round() as u32;
        if self.texture_state == Some((self.page_index, zoom_key)) {
            return; // déjà à jour.
        }

        let result = (|| -> pdf_core::Result<egui::ColorImage> {
            let page = doc.page(self.page_index)?;
            let content = doc.page_content(&page)?;
            let display =
                pdf_core::interp::Interpreter::run_page(doc, page.resources.clone(), &content)?;
            let pixmap = pdf_render::render_page_scaled(&display, page.media_box, self.zoom as f64)
                .ok_or_else(|| {
                    pdf_core::PdfError::InvalidObject(0, "render target allocation failed".into())
                })?;
            Ok(egui::ColorImage::from_rgba_unmultiplied(
                [pixmap.width() as usize, pixmap.height() as usize],
                pixmap.data(),
            ))
        })();

        match result {
            Ok(image) => {
                self.texture = Some(ctx.load_texture("page", image, egui::TextureOptions::LINEAR));
                self.texture_state = Some((self.page_index, zoom_key));
                self.error = None;
            }
            Err(e) => {
                self.error = Some(format!("Erreur de rendu page {}: {e}", self.page_index));
                self.texture = None;
            }
        }
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

                let has_doc = self.doc.is_some();
                ui.add_enabled_ui(has_doc && self.page_index > 0, |ui| {
                    if ui.button("◀ Précédente").clicked() {
                        self.page_index -= 1;
                    }
                });
                ui.label(if has_doc {
                    format!("Page {} / {}", self.page_index + 1, self.page_count.max(1))
                } else {
                    "Aucun document".to_string()
                });
                ui.add_enabled_ui(has_doc && self.page_index + 1 < self.page_count, |ui| {
                    if ui.button("Suivante ▶").clicked() {
                        self.page_index += 1;
                    }
                });

                ui.separator();

                ui.add_enabled_ui(has_doc, |ui| {
                    if ui.button("－").clicked() {
                        self.zoom = (self.zoom - 0.25).max(0.25);
                    }
                    ui.label(format!("{:.0}%", self.zoom * 100.0));
                    if ui.button("＋").clicked() {
                        self.zoom = (self.zoom + 0.25).min(4.0);
                    }
                    if ui.button("Réinitialiser").clicked() {
                        self.zoom = 1.0;
                    }
                });

                if let Some(path) = &self.path {
                    ui.separator();
                    ui.label(path.file_name().unwrap_or_default().to_string_lossy());
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(err) = &self.error {
                ui.colored_label(egui::Color32::RED, err);
                return;
            }
            if self.doc.is_none() {
                ui.centered_and_justified(|ui| {
                    ui.label("Ouvrez un fichier PDF pour commencer.");
                });
                return;
            }

            self.ensure_texture(ctx);

            if let Some(texture) = &self.texture {
                egui::ScrollArea::both().show(ui, |ui| {
                    ui.image(texture);
                });
            }
        });
    }
}
