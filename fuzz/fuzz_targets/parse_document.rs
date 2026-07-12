#![no_main]

use libfuzzer_sys::fuzz_target;

// Fuzz cible principale : `Document::open` sur des octets arbitraires,
// puis un balayage superficiel de tout ce qui ne nécessite pas de rendu
// (nombre de pages, contenu de chaque page, table des matières). Couvre
// lexer/xref/parser/filtres/déchiffrement (`pdf-core::crypt`) — la surface
// d'attaque la plus exposée (n'importe quel fichier ouvert par l'utilisateur
// passe par là), sans le coût du rendu (voir `render_document.rs` pour
// ça). N'échoue jamais sur un `Result::Err` — un PDF malformé doit renvoyer
// une erreur propre, pas paniquer ; c'est uniquement la panique/le crash que
// `cargo fuzz` détecte.
fuzz_target!(|data: &[u8]| {
    let Ok(doc) = pdf_core::Document::open(data.to_vec()) else {
        return;
    };
    let Ok(page_count) = doc.page_count() else {
        return;
    };
    // Borné : un PDF malformé peut prétendre avoir un nombre de pages
    // gigantesque sans que ça reflète le contenu réel du fichier.
    for index in 0..page_count.min(50) {
        let Ok(page) = doc.page(index) else {
            continue;
        };
        let _ = doc.page_content(&page);
    }
    let _ = doc.outline();
});
