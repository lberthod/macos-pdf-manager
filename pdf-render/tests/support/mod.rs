//! Support partagé pour les tests de non-régression pixel (harnais de
//! comparaison d'images, sprint.md Sprint 0/Sprint 7-8). Rendu déterministe :
//! `pdf-render` ne dépend d'aucune source d'aléa, donc une image de
//! référence ("golden") ne devrait différer d'un nouveau rendu que si le
//! comportement de rendu a réellement changé (régression ou évolution
//! volontaire).
use pdf_core::interp::Interpreter;
use pdf_core::Document;
use tiny_skia::Pixmap;

pub struct DiffStats {
    pub max_channel_delta: u8,
    pub differing_pixels: usize,
    pub total_pixels: usize,
}

/// Différence pixel par pixel entre deux images de mêmes dimensions. Un
/// pixel est compté comme "différent" seulement si l'écart sur au moins un
/// canal dépasse `PIXEL_TOLERANCE`, pour absorber le bruit d'arrondi flottant
/// sans masquer une vraie régression de rendu.
const PIXEL_TOLERANCE: u8 = 2;

pub fn diff(a: &Pixmap, b: &Pixmap) -> Result<DiffStats, String> {
    if a.width() != b.width() || a.height() != b.height() {
        return Err(format!(
            "dimension mismatch: {}x{} vs {}x{}",
            a.width(),
            a.height(),
            b.width(),
            b.height()
        ));
    }
    let da = a.data();
    let db = b.data();
    let total_pixels = (a.width() * a.height()) as usize;
    let mut max_channel_delta = 0u8;
    let mut differing_pixels = 0usize;
    for px in 0..total_pixels {
        let o = px * 4;
        let mut pixel_differs = false;
        for c in 0..4 {
            let d = da[o + c].abs_diff(db[o + c]);
            if d > max_channel_delta {
                max_channel_delta = d;
            }
            if d > PIXEL_TOLERANCE {
                pixel_differs = true;
            }
        }
        if pixel_differs {
            differing_pixels += 1;
        }
    }
    Ok(DiffStats {
        max_channel_delta,
        differing_pixels,
        total_pixels,
    })
}

/// Ouvre un PDF depuis ses octets bruts, interprète le contenu de sa
/// première page et la rasterise à l'échelle 1:1 (`/Rotate` appliqué).
pub fn render_first_page(pdf_bytes: &[u8]) -> Pixmap {
    let doc = Document::open(pdf_bytes.to_vec()).expect("document should open");
    let page = doc.page(0).expect("page 0 should exist");
    let content = doc.page_content(&page).expect("page content should decode");
    let display = Interpreter::run_page(&doc, page.resources.clone(), &content)
        .expect("content stream should interpret");
    pdf_render::render_page_rotated(&display, page.media_box, page.rotate, 1.0)
        .expect("render should produce a pixmap")
}

/// Compare `pixmap` à l'image de référence `tests/golden/<name>.png`.
///
/// - Si l'image de référence n'existe pas encore, elle est créée à partir du
///   rendu courant (bootstrap d'un nouveau fixture) et le test passe : à
///   relire manuellement dans le diff de PR avant de merger.
/// - Sinon, échoue si l'écart dépasse le seuil défini ci-dessous. En cas
///   d'échec, l'image obtenue est écrite à côté (`<name>.actual.png`) pour
///   inspection. Pour mettre à jour volontairement une référence après un
///   changement de rendu voulu : `UPDATE_GOLDEN=1 cargo test -p pdf-render`.
pub fn assert_matches_golden(name: &str, pixmap: &Pixmap) {
    let golden_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    std::fs::create_dir_all(&golden_dir).unwrap();
    let golden_path = golden_dir.join(format!("{name}.png"));

    if std::env::var_os("UPDATE_GOLDEN").is_some() || !golden_path.exists() {
        std::fs::write(&golden_path, pixmap.encode_png().unwrap()).unwrap();
        return;
    }

    let golden_bytes = std::fs::read(&golden_path).unwrap();
    let golden = Pixmap::decode_png(&golden_bytes)
        .unwrap_or_else(|e| panic!("failed to decode golden image for '{name}': {e}"));

    let stats =
        diff(pixmap, &golden).unwrap_or_else(|e| panic!("golden mismatch for '{name}': {e}"));

    const MAX_CHANNEL_DELTA: u8 = 6;
    const MAX_DIFFERING_RATIO: f64 = 0.005; // 0.5% des pixels

    let differing_ratio = stats.differing_pixels as f64 / stats.total_pixels as f64;
    if stats.max_channel_delta > MAX_CHANNEL_DELTA || differing_ratio > MAX_DIFFERING_RATIO {
        let actual_path = golden_dir.join(format!("{name}.actual.png"));
        std::fs::write(&actual_path, pixmap.encode_png().unwrap()).unwrap();
        panic!(
            "pixel diff for '{name}' exceeds threshold: max_channel_delta={} (limit {}), \
             differing_pixels={}/{} ({:.3}%, limit {:.3}%). Actual image written to {}. \
             If this is an intentional rendering change, rerun with UPDATE_GOLDEN=1.",
            stats.max_channel_delta,
            MAX_CHANNEL_DELTA,
            stats.differing_pixels,
            stats.total_pixels,
            differing_ratio * 100.0,
            MAX_DIFFERING_RATIO * 100.0,
            actual_path.display()
        );
    }
}
