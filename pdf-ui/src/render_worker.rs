//! Rendu en arrière-plan (Sprint 21, #20) : un thread dédié rastérise les
//! pages "hors écran" (miniatures, défilement continu) pour que l'affichage
//! d'un gros document (des centaines de pages) ne bloque jamais la frame
//! `egui` le temps de rastériser une page qui vient d'entrer dans la zone
//! visible. La page courante en mode "page unique" reste rendue de façon
//! synchrone (`ViewerApp::ensure_texture`) : une seule page à la fois, le
//! risque de blocage y est minime, et l'afficher immédiatement plutôt qu'en
//! différé évite un scintillement au changement de page.
//!
//! Le thread ne partage aucun état avec `pdf_app::Session` (qui utilise
//! `Rc`/`RefCell`, non `Send`) : il reçoit les octets bruts du document
//! courant (`RenderWorker::set_document`, envoyés à l'ouverture et après
//! chaque édition, voir `Session::current_bytes`) et reparse son propre
//! `pdf_core::Document` en interne — aucun état partagé, seulement des
//! messages `Send` (`Vec<u8>`, entiers). Rendu **toujours** via `pdf-render`
//! (CPU), jamais le backend GPU (`pdf-render-gpu`) : simplification
//! délibérée pour éviter de partager un `Device`/`Queue` `wgpu` entre
//! threads sans bénéfice mesurable ici — la parité pixel CPU/GPU est déjà
//! vérifiée par `pdf-render-gpu/tests/cross_backend.rs`, donc le résultat
//! affiché reste visuellement équivalent.

use std::sync::mpsc::{channel, Receiver, Sender};

/// Distingue à quel cache `pdf-ui` doit livrer une page rastérisée en
/// arrière-plan — miniatures (`ViewerApp::thumbnails`) ou pages du
/// défilement continu (`ViewerApp::page_textures`), qui partagent le même
/// thread/canal mais pas le même `HashMap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenderKind {
    Thumbnail,
    ContinuousPage,
}

/// Une page rastérisée par le thread d'arrière-plan, prête à devenir une
/// texture `egui` — mêmes champs que `pdf_app::RenderedPage`, dupliqués ici
/// pour ne pas faire dépendre ce module de `pdf-app` (le worker ne connaît
/// que `pdf-core`/`pdf-render`).
pub struct BackgroundRenderedPage {
    pub kind: RenderKind,
    pub page_index: usize,
    pub scale_key: u32,
    pub generation: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

enum Request {
    SetDocument {
        bytes: Vec<u8>,
        generation: u64,
    },
    Render {
        kind: RenderKind,
        page_index: usize,
        scale: f64,
        scale_key: u32,
        generation: u64,
    },
}

/// Poignée conservée par `ViewerApp` : envoie des requêtes de rendu et
/// récupère les résultats déjà prêts, sans jamais bloquer l'appelant.
pub struct RenderWorker {
    requests: Sender<Request>,
    results: Receiver<BackgroundRenderedPage>,
}

impl RenderWorker {
    /// Démarre le thread d'arrière-plan — un seul par `ViewerApp`, pour
    /// toute la durée de vie de l'application (voir `ViewerApp::new`).
    pub fn spawn() -> Self {
        let (req_tx, req_rx) = channel::<Request>();
        let (res_tx, res_rx) = channel::<BackgroundRenderedPage>();

        std::thread::spawn(move || worker_loop(req_rx, res_tx));

        Self {
            requests: req_tx,
            results: res_rx,
        }
    }

    /// Remplace le document que le thread rastérise — à appeler à
    /// l'ouverture d'un fichier et après chaque édition (`generation`
    /// incrémenté à chaque appel, voir `ViewerApp::refresh_render_worker_document`) :
    /// toute requête de rendu portant une `generation` différente de la
    /// dernière reçue par le thread est silencieusement ignorée, pour ne
    /// jamais afficher une page rastérisée depuis un contenu déjà périmé.
    pub fn set_document(&self, bytes: Vec<u8>, generation: u64) {
        let _ = self
            .requests
            .send(Request::SetDocument { bytes, generation });
    }

    /// Demande le rendu de `page_index` à `scale` — asynchrone, le résultat
    /// (s'il arrive) sera renvoyé par un futur appel à `drain_results`.
    pub fn request_render(
        &self,
        kind: RenderKind,
        page_index: usize,
        scale: f64,
        scale_key: u32,
        generation: u64,
    ) {
        let _ = self.requests.send(Request::Render {
            kind,
            page_index,
            scale,
            scale_key,
            generation,
        });
    }

    /// Récupère toutes les pages rastérisées disponibles depuis le dernier
    /// appel (non bloquant) — à appeler une fois par frame.
    pub fn drain_results(&self) -> Vec<BackgroundRenderedPage> {
        let mut out = Vec::new();
        while let Ok(page) = self.results.try_recv() {
            out.push(page);
        }
        out
    }
}

fn worker_loop(requests: Receiver<Request>, results: Sender<BackgroundRenderedPage>) {
    let mut doc: Option<pdf_core::Document> = None;
    let mut generation: u64 = 0;

    for request in requests {
        // AUDIT_FIXES_2026-09-03.md #12 : sans ce `catch_unwind`, un panic
        // pendant le rendu d'une seule page (ex. déclencheur direct #2, une
        // `/MediaBox` forgée) faisait mourir le thread entier en silence —
        // miniatures et défilement continu figés sans message, et toute
        // requête suivante avalée sans effet par le `let _ =
        // self.requests.send(...)` côté `RenderWorker` (le canal reste
        // ouvert côté émetteur même si personne ne le lit plus). Récupérer
        // le panic ici laisse le thread continuer à servir les requêtes
        // suivantes plutôt que de disparaître.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            process_request(request, &mut doc, &mut generation, &results)
        }));
        if let Err(panic) = outcome {
            eprintln!(
                "pdf-ui render worker: recovered from a panic while rendering a page: {}",
                panic_message(&panic)
            );
        }
    }
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

fn process_request(
    request: Request,
    doc: &mut Option<pdf_core::Document>,
    generation: &mut u64,
    results: &Sender<BackgroundRenderedPage>,
) {
    match request {
        Request::SetDocument {
            bytes,
            generation: new_generation,
        } => {
            *doc = pdf_core::Document::open(bytes).ok();
            *generation = new_generation;
        }
        Request::Render {
            kind,
            page_index,
            scale,
            scale_key,
            generation: request_generation,
        } => {
            if request_generation != *generation {
                return; // requête périmée : le document a déjà changé depuis.
            }
            let Some(doc) = doc.as_ref() else { return };
            let Ok(page) = doc.page(page_index) else {
                return;
            };
            let Ok(content) = doc.page_content(&page) else {
                return;
            };
            let Ok(display) =
                pdf_core::interp::Interpreter::run_page_with_annotations(doc, &page, &content)
            else {
                return;
            };
            let Some(pixmap) =
                pdf_render::render_page_rotated(&display, page.media_box, page.rotate, scale)
            else {
                return;
            };
            let _ = results.send(BackgroundRenderedPage {
                kind,
                page_index,
                scale_key,
                generation: request_generation,
                width: pixmap.width(),
                height: pixmap.height(),
                rgba: pixmap.data().to_vec(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_message_extracts_string_and_str_payloads() {
        let str_payload: Box<dyn std::any::Any + Send> = Box::new("boom");
        assert_eq!(panic_message(str_payload.as_ref()), "boom");

        let string_payload: Box<dyn std::any::Any + Send> = Box::new("boom".to_string());
        assert_eq!(panic_message(string_payload.as_ref()), "boom");

        let other_payload: Box<dyn std::any::Any + Send> = Box::new(42i32);
        assert_eq!(
            panic_message(other_payload.as_ref()),
            "non-string panic payload"
        );
    }

    /// AUDIT_FIXES_2026-09-03.md #12 : la boucle du thread doit continuer à
    /// traiter les requêtes suivantes après qu'une requête a paniqué, pas
    /// s'arrêter — reproduit ici la structure exacte de `worker_loop`
    /// (`catch_unwind` par itération) sans dépendre d'un vrai déclencheur de
    /// panic dans le pipeline de rendu (le déclencheur connu, #2, est
    /// justement corrigé pour ne plus paniquer — voir la doc de module de
    /// `worker_loop`).
    #[test]
    fn catch_unwind_per_request_lets_the_loop_keep_processing_after_a_panic() {
        let mut processed_after_panic = false;
        for i in 0..3 {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if i == 1 {
                    panic!("simulated render panic");
                }
            }));
            if outcome.is_err() && i == 1 {
                continue;
            }
            if i == 2 {
                processed_after_panic = true;
            }
        }
        assert!(
            processed_after_panic,
            "the loop must keep running after a panicking iteration"
        );
    }
}
