//! Harnais de comparaison pixel (diff + seuil) sur le corpus de fixtures —
//! ferme le trou identifié dès le Sprint 0 et rouvert au critère de sortie
//! du Sprint 7-8 dans sprint.md : ce fichier est la première suite de tests
//! de rendu comparée à une image de référence plutôt qu'à une simple
//! assertion "ça n'a pas planté". Voir `tests/support/mod.rs` pour le
//! mécanisme de diff/seuil et `tests/golden/` pour les images de référence.
//!
//! `encrypted_rc4.pdf` n'est pas couvert ici : `Document::open` doit y
//! échouer par construction (voir les tests dédiés dans `pdf-core`), il n'y
//! a donc rien à rendre.

mod support;

use support::{assert_matches_golden, render_first_page};

macro_rules! golden_test {
    ($test_name:ident, $fixture:expr) => {
        #[test]
        fn $test_name() {
            let bytes = include_bytes!(concat!("../../pdf-core/tests/fixtures/", $fixture));
            let pixmap = render_first_page(bytes);
            assert_matches_golden(stringify!($test_name), &pixmap);
        }
    };
}

golden_test!(golden_minimal, "minimal.pdf");
golden_test!(golden_multipage_classic_xref, "multipage_classic_xref.pdf");
golden_test!(golden_multipage_xref_stream, "multipage_xref_stream.pdf");
golden_test!(golden_corrupted_missing_xref, "corrupted_missing_xref.pdf");
golden_test!(golden_embedded_truetype_font, "embedded_truetype_font.pdf");
golden_test!(golden_image_jpeg, "image_jpeg.pdf");
golden_test!(golden_embedded_cff_font, "embedded_cff_font.pdf");
golden_test!(golden_image_smask, "image_smask.pdf");
golden_test!(golden_rotated_page, "rotated_page.pdf");
golden_test!(golden_acroform_textfield, "acroform_textfield.pdf");
golden_test!(golden_cjk_text, "cjk_text.pdf");
golden_test!(golden_large_60_pages, "large_60_pages.pdf");
golden_test!(golden_outline_test, "outline_test.pdf");
golden_test!(golden_type0_cid_truetype, "type0_cid_truetype.pdf");
golden_test!(golden_type0_cid_cff, "type0_cid_cff.pdf");
golden_test!(
    golden_bold_italic_standard_fonts,
    "bold_italic_standard_fonts.pdf"
);
golden_test!(
    golden_landscape_mixed_page_sizes,
    "landscape_mixed_page_sizes.pdf"
);
golden_test!(golden_cmyk_jpeg, "cmyk_jpeg.pdf");
golden_test!(
    golden_incremental_updates_chain,
    "incremental_updates_chain.pdf"
);
golden_test!(golden_malformed_wrong_length, "malformed_wrong_length.pdf");
golden_test!(golden_indexed_color_image, "indexed_color_image.pdf");
golden_test!(golden_scanned_page_like, "scanned_page_like.pdf");
golden_test!(golden_pdfa_like_minimal, "pdfa_like_minimal.pdf");
