//! Comparaison pixel automatisée entre `pdf-render` (CPU, `tiny-skia`) et
//! `pdf-render-gpu` (`wgpu`) sur le corpus de fixtures réels — comble le
//! trou signalé dans sprint.md (Sprint 9-10) : jusqu'ici la parité entre les
//! deux back-ends n'était vérifiée que par quelques assertions de pixels
//! ciblées (voir `src/lib.rs::tests`), pas par un vrai diff page entière.
//!
//! Les deux rasteriseurs ont des pipelines différents (rastérisation
//! logicielle vs tessellation `lyon` + stencil GPU) : un écart d'anti-
//! aliasing de quelques niveaux par canal sur les bords est attendu et
//! toléré, mais un écart massif (mauvaise position, couleur, page vide)
//! ne l'est pas.
//!
//! Si aucun adaptateur `wgpu` n'est disponible dans l'environnement (CI sans
//! GPU/Metal), les tests s'auto-neutralisent (`eprintln!` + retour anticipé)
//! plutôt que d'échouer, comme le reste de la suite `pdf-render-gpu`.

use pdf_core::display::DisplayList;
use pdf_core::interp::Interpreter;
use pdf_core::Document;
use pdf_render_gpu::GpuRenderer;

fn build_display(pdf_bytes: &[u8]) -> (DisplayList, [f64; 4], i32) {
    let doc = Document::open(pdf_bytes.to_vec()).expect("document should open");
    let page = doc.page(0).expect("page 0 should exist");
    let content = doc.page_content(&page).expect("page content should decode");
    let display = Interpreter::run_page(&doc, page.resources.clone(), &content)
        .expect("content stream should interpret");
    (display, page.media_box, page.rotate)
}

/// Fraction de pixels dont au moins un canal diffère de plus de
/// `channel_tolerance` entre les deux images (mêmes dimensions requises).
fn differing_ratio(cpu: &[u8], gpu: &[u8], channel_tolerance: u8) -> f64 {
    assert_eq!(
        cpu.len(),
        gpu.len(),
        "buffer size mismatch between backends"
    );
    let total_pixels = cpu.len() / 4;
    let mut differing = 0usize;
    for px in 0..total_pixels {
        let o = px * 4;
        let pixel_differs = (0..4).any(|c| cpu[o + c].abs_diff(gpu[o + c]) > channel_tolerance);
        if pixel_differs {
            differing += 1;
        }
    }
    differing as f64 / total_pixels as f64
}

macro_rules! cross_backend_test {
    ($test_name:ident, $fixture:expr, $max_differing_ratio:expr) => {
        #[test]
        fn $test_name() {
            let Some(renderer) = GpuRenderer::new() else {
                eprintln!("no wgpu adapter available in this environment, skipping");
                return;
            };

            let bytes = include_bytes!(concat!("../../pdf-core/tests/fixtures/", $fixture));
            let (display, media_box, rotate) = build_display(bytes);

            let cpu = pdf_render::render_page_rotated(&display, media_box, rotate, 1.0)
                .expect("CPU render should succeed");
            let gpu = renderer
                .render_page_rotated(&display, media_box, rotate, 1.0)
                .expect("GPU render should succeed");

            assert_eq!(
                (cpu.width(), cpu.height()),
                (gpu.width, gpu.height),
                "CPU and GPU backends disagree on output dimensions for '{}'",
                $fixture
            );

            let ratio = differing_ratio(cpu.data(), &gpu.rgba, 24);
            assert!(
                ratio <= $max_differing_ratio,
                "CPU/GPU pixel mismatch for '{}': {:.3}% of pixels differ (limit {:.3}%)",
                $fixture,
                ratio * 100.0,
                $max_differing_ratio * 100.0
            );
        }
    };
}

// Seuils un peu plus larges que les images filles/texte (bords anti-aliasés
// plus nombreux, densité de traits plus élevée).
cross_backend_test!(cross_backend_minimal, "minimal.pdf", 0.01);
cross_backend_test!(
    cross_backend_multipage_classic_xref,
    "multipage_classic_xref.pdf",
    0.02
);
cross_backend_test!(
    cross_backend_embedded_truetype_font,
    "embedded_truetype_font.pdf",
    0.02
);
cross_backend_test!(
    cross_backend_embedded_cff_font,
    "embedded_cff_font.pdf",
    0.02
);
cross_backend_test!(cross_backend_rotated_page, "rotated_page.pdf", 0.02);
cross_backend_test!(cross_backend_image_jpeg, "image_jpeg.pdf", 0.03);
cross_backend_test!(cross_backend_image_smask, "image_smask.pdf", 0.03);
cross_backend_test!(
    cross_backend_type0_cid_truetype,
    "type0_cid_truetype.pdf",
    0.02
);
cross_backend_test!(cross_backend_type0_cid_cff, "type0_cid_cff.pdf", 0.02);
cross_backend_test!(
    cross_backend_bold_italic_standard_fonts,
    "bold_italic_standard_fonts.pdf",
    0.02
);
cross_backend_test!(cross_backend_cmyk_jpeg, "cmyk_jpeg.pdf", 0.03);
cross_backend_test!(
    cross_backend_scanned_page_like,
    "scanned_page_like.pdf",
    0.03
);
