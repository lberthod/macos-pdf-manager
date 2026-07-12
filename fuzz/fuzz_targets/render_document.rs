#![no_main]

use libfuzzer_sys::fuzz_target;
use pdf_core::interp::Interpreter;

// Comme `parse_document.rs`, mais va jusqu'au rendu CPU (`pdf-render`) de
// la première page — couvre en plus l'interpréteur de flux de contenu, la
// résolution de polices (TrueType/CFF/CID, y compris la substitution
// système) et le décodage d'image (JPEG/filtres), le tout par-dessus le
// parsing déjà couvert par `parse_document.rs`. Plus lent (rasterisation
// réelle), donc limité à la première page plutôt qu'à tout le document.
fuzz_target!(|data: &[u8]| {
    let Ok(doc) = pdf_core::Document::open(data.to_vec()) else {
        return;
    };
    let Ok(page_count) = doc.page_count() else {
        return;
    };
    if page_count == 0 {
        return;
    }
    let Ok(page) = doc.page(0) else {
        return;
    };
    let Ok(content) = doc.page_content(&page) else {
        return;
    };
    let Ok(display) = Interpreter::run_page_with_annotations(&doc, &page, &content) else {
        return;
    };
    let _ = pdf_render::render_page(&display, page.media_box);
});
