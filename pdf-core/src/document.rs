//! Modèle document (arbre logique) — architecture.md §4.4.

use crate::error::{PdfError, Result};
use crate::filters::decode_stream;
use crate::object::{Dictionary, ObjRef, Object};
use crate::parser::Parser;
use crate::xref::{parse_xref_chain, XrefEntry, XrefTable};
use std::cell::RefCell;
use std::collections::BTreeMap;

pub struct Document {
    data: Vec<u8>,
    xref: XrefTable,
    trailer: Dictionary,
    cache: RefCell<BTreeMap<u32, Object>>,
}

impl Document {
    pub fn open(data: Vec<u8>) -> Result<Self> {
        let (xref, trailer) = parse_xref_chain(&data)?;
        // `/Encrypt` (RC4/AES, ISO 32000-1 §7.6) n'est pas implémenté : sans
        // ce contrôle explicite, les flux/chaînes restent chiffrés et
        // échouent plus loin avec des erreurs de bas niveau trompeuses
        // (ex. "FlateDecode: corrupt deflate stream" au lieu de la vraie
        // cause). Voir sprint.md Sprint 18+ (chiffrement).
        if trailer.get("Encrypt").is_some() {
            return Err(PdfError::Encrypted);
        }
        Ok(Self {
            data,
            xref,
            trailer,
            cache: RefCell::new(BTreeMap::new()),
        })
    }

    pub fn object_count(&self) -> usize {
        self.xref.entries.len()
    }

    pub fn trailer(&self) -> &Dictionary {
        &self.trailer
    }

    /// Résout un objet indirect par numéro (la génération n'est pas encore
    /// vérifiée : suffisant tant que les PDF avec objets libérés/réutilisés
    /// ne sont pas dans le corpus de test prioritaire). Gère à la fois les
    /// objets à offset direct et les objets compressés dans un object
    /// stream (`/Type /ObjStm`, PDF 1.5+).
    pub fn resolve(&self, r: ObjRef) -> Result<Object> {
        if let Some(cached) = self.cache.borrow().get(&r.num) {
            return Ok(cached.clone());
        }
        let entry = *self
            .xref
            .entries
            .get(&r.num)
            .ok_or(PdfError::ObjectNotFound(r.num, r.gen))?;

        let object = match entry {
            XrefEntry::Offset(offset) => {
                let mut parser = Parser::with_pos(&self.data, offset);
                let (_num, _gen, object) = parser.parse_indirect_object()?;
                object
            }
            XrefEntry::Compressed { stream_num, index } => {
                self.resolve_compressed(stream_num, index)?
            }
        };

        self.cache.borrow_mut().insert(r.num, object.clone());
        Ok(object)
    }

    /// Extrait l'objet d'indice `index` d'un object stream (`/Type /ObjStm`) —
    /// architecture.md §4.2. L'en-tête du flux décodé liste `/N` paires
    /// `(numéro d'objet, offset relatif à /First)`.
    fn resolve_compressed(&self, stream_num: u32, index: u32) -> Result<Object> {
        let stream_obj = self.resolve(ObjRef::new(stream_num, 0))?;
        let Object::Stream(stream) = stream_obj else {
            return Err(PdfError::InvalidObject(
                0,
                format!("object {stream_num} is not an object stream"),
            ));
        };
        let n = stream.dict.get_int("N")?;
        let first = stream.dict.get_int("First")?;
        let decoded = decode_stream(&stream)?;

        let mut header_parser = Parser::new(&decoded);
        let mut rel_offset = None;
        for i in 0..n {
            let num = header_parser
                .parse_object()?
                .as_int()
                .ok_or(PdfError::UnexpectedType("Integer"))?;
            let off = header_parser
                .parse_object()?
                .as_int()
                .ok_or(PdfError::UnexpectedType("Integer"))?;
            if i as u32 == index {
                rel_offset = Some(off as usize);
                let _ = num; // le numéro d'objet est déjà connu via la xref.
            }
        }
        let rel_offset = rel_offset.ok_or(PdfError::ObjectNotFound(stream_num, 0))?;

        let mut obj_parser = Parser::with_pos(&decoded, first as usize + rel_offset);
        obj_parser.parse_object()
    }

    /// Retourne l'objet directement si ce n'est pas une référence, ou le
    /// résout sinon. Point d'entrée pratique pour naviguer le graphe.
    pub fn get(&self, object: &Object) -> Result<Object> {
        match object {
            Object::Reference(r) => self.resolve(*r),
            other => Ok(other.clone()),
        }
    }

    pub fn root(&self) -> Result<Dictionary> {
        let root_obj = self
            .trailer
            .get("Root")
            .ok_or_else(|| PdfError::MissingKey("Root".into()))?;
        let root = self.get(root_obj)?;
        root.as_dict()
            .cloned()
            .ok_or(PdfError::UnexpectedType("Dictionary"))
    }

    /// Nombre de pages, obtenu via un parcours réel de l'arbre `/Pages`
    /// (voir `page.rs`, Sprint 5-6) plutôt que la simple lecture de
    /// `/Count` (qui peut être absente ou incohérente sur des PDF malformés).
    pub fn page_count(&self) -> Result<usize> {
        Ok(self.pages()?.len())
    }

    pub fn metadata_dict(&self) -> Option<Dictionary> {
        let info_obj = self.trailer.get("Info")?;
        let info = self.get(info_obj).ok()?;
        info.as_dict().cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PDF minimal valide (une page vide) construit à la main pour les tests
    /// end-to-end. Offsets calculés pour correspondre à la xref ci-dessous.
    fn minimal_pdf() -> Vec<u8> {
        let body = concat!(
            "%PDF-1.7\n",
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
            "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let mut bytes = body.as_bytes().to_vec();

        // Calcule les offsets réels de chaque "N 0 obj".
        let offset_of = |data: &[u8], needle: &str| -> usize {
            data.windows(needle.len())
                .position(|w| w == needle.as_bytes())
                .unwrap()
        };
        let off1 = offset_of(&bytes, "1 0 obj");
        let off2 = offset_of(&bytes, "2 0 obj");
        let off3 = offset_of(&bytes, "3 0 obj");

        let xref_offset = bytes.len();
        let xref = format!(
            "xref\n0 4\n0000000000 65535 f \n{:010} 00000 n \n{:010} 00000 n \n{:010} 00000 n \ntrailer\n<< /Size 4 /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
            off1, off2, off3, xref_offset
        );
        bytes.extend_from_slice(xref.as_bytes());
        bytes
    }

    #[test]
    fn opens_minimal_pdf_and_resolves_page_count() {
        let doc = Document::open(minimal_pdf()).unwrap();
        assert_eq!(doc.object_count(), 3);
        assert_eq!(doc.page_count().unwrap(), 1);
        let root = doc.root().unwrap();
        assert_eq!(root.get("Type").unwrap().as_name(), Some("Catalog"));
    }

    #[test]
    fn falls_back_to_reconstruction_without_valid_xref() {
        // Même contenu d'objets, mais xref/trailer volontairement absents :
        // le scanner de secours doit tout de même trouver les objets, même
        // si sans trailer explicite le Root/page_count restent indisponibles.
        let body = concat!(
            "%PDF-1.7\n",
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
            "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        );
        let broken = body.as_bytes().to_vec();
        let doc = Document::open(broken).unwrap();
        assert_eq!(doc.object_count(), 2);
    }

    /// Fixtures réels (générés via reportlab + pikepdf, voir
    /// `tests/fixtures/README.md`) couvrant xref classique, cross-reference
    /// streams + object streams (PDF 1.5+), et un fichier corrompu.
    #[test]
    fn opens_real_pdf_with_classic_xref() {
        let bytes = include_bytes!("../tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let doc = Document::open(bytes).unwrap();
        assert_eq!(doc.page_count().unwrap(), 5);
    }

    #[test]
    fn opens_real_pdf_with_xref_stream_and_object_streams() {
        let bytes = include_bytes!("../tests/fixtures/multipage_xref_stream.pdf").to_vec();
        let doc = Document::open(bytes).unwrap();
        assert_eq!(doc.page_count().unwrap(), 5);
        let root = doc.root().unwrap();
        assert_eq!(root.get("Type").unwrap().as_name(), Some("Catalog"));
    }

    #[test]
    fn recovers_real_pdf_missing_xref_via_catalog_scan() {
        let bytes = include_bytes!("../tests/fixtures/corrupted_missing_xref.pdf").to_vec();
        let doc = Document::open(bytes).unwrap();
        assert_eq!(doc.page_count().unwrap(), 5);
    }

    /// Corpus élargi (voir `tests/fixtures/README.md`) : PDF avec des
    /// caractéristiques avancées non encore supportées nativement, mais qui
    /// doivent au moins s'ouvrir sans paniquer et donner un comportement
    /// documenté (succès dégradé ou erreur claire, jamais un panic ou une
    /// erreur de bas niveau trompeuse).
    #[test]
    fn opens_pdf_with_rotate_and_exposes_it_on_the_page() {
        let bytes = include_bytes!("../tests/fixtures/rotated_page.pdf").to_vec();
        let doc = Document::open(bytes).unwrap();
        let page = doc.page(0).unwrap();
        assert_eq!(page.rotate, 90);
    }

    #[test]
    fn opens_pdf_with_acroform_without_crashing() {
        // Le champ de formulaire n'est pas rendu comme un widget interactif
        // (pdf-edit ne gère pas encore les AcroForm), mais le texte de la
        // page doit tout de même être extrait normalement : le formulaire
        // ne doit pas faire échouer le reste du pipeline.
        let bytes = include_bytes!("../tests/fixtures/acroform_textfield.pdf").to_vec();
        let doc = Document::open(bytes).unwrap();
        assert_eq!(doc.page_count().unwrap(), 1);
        let root = doc.root().unwrap();
        assert!(
            root.get("AcroForm").is_some(),
            "fixture should actually contain an AcroForm entry"
        );
    }

    /// `/Encrypt` (RC4/AES) n'est pas supporté : `Document::open` doit
    /// échouer avec une erreur claire (`PdfError::Encrypted`), pas avec un
    /// message de bas niveau trompeur comme une erreur `FlateDecode` sur du
    /// contenu resté chiffré.
    #[test]
    fn rejects_encrypted_pdf_with_a_clear_error() {
        let bytes = include_bytes!("../tests/fixtures/encrypted_rc4.pdf").to_vec();
        match Document::open(bytes) {
            Err(PdfError::Encrypted) => {}
            Err(other) => panic!("expected PdfError::Encrypted, got {other:?}"),
            Ok(_) => panic!("expected an error, encrypted PDF opened successfully"),
        }
    }

    /// Symétrique de `rejects_encrypted_pdf_with_a_clear_error` pour un
    /// filtre de chiffrement différent (AES-256/R=6, `pikepdf.Encryption(...,
    /// aes=True)`) plutôt que le seul RC4 40 bits déjà couvert : les deux
    /// doivent échouer de la même façon claire, pas seulement le premier
    /// chemin de code de déchiffrement rencontré.
    #[test]
    fn rejects_aes256_encrypted_pdf_with_a_clear_error() {
        let bytes = include_bytes!("../tests/fixtures/encrypted_aes256.pdf").to_vec();
        match Document::open(bytes) {
            Err(PdfError::Encrypted) => {}
            Err(other) => panic!("expected PdfError::Encrypted, got {other:?}"),
            Ok(_) => panic!("expected an error, encrypted PDF opened successfully"),
        }
    }

    /// Trois mises à jour incrémentales chaînées (`/Prev -> /Prev -> /Prev`),
    /// contre le seul niveau simple déjà couvert par
    /// `corrupted_missing_xref.pdf` : la chaîne complète doit rester
    /// résolvable et le contenu de chaque révision (accumulé, pas remplacé)
    /// doit rester lisible.
    #[test]
    fn resolves_a_three_level_incremental_update_chain() {
        let bytes = include_bytes!("../tests/fixtures/incremental_updates_chain.pdf").to_vec();
        let doc = Document::open(bytes).unwrap();
        assert_eq!(doc.page_count().unwrap(), 1);
        let page = doc.page(0).unwrap();
        let content = doc.page_content(&page).unwrap();
        let text = String::from_utf8_lossy(&content);
        for revision in ["Revision 1", "Revision 2", "Revision 3", "Revision 4"] {
            assert!(
                text.contains(revision),
                "expected content stream to contain '{revision}', got: {text}"
            );
        }
    }

    /// Un document dont chaque page a une `/MediaBox` différente (portrait
    /// Letter, paysage A4, carré) doit ouvrir et exposer la bonne taille par
    /// page — condition posée par la limitation connue du défilement continu
    /// de `pdf-ui` (hauteur de ligne dérivée de la page 0 uniquement, voir
    /// sprint.md Sprint 9-10).
    #[test]
    fn opens_document_with_differently_sized_pages() {
        let bytes = include_bytes!("../tests/fixtures/landscape_mixed_page_sizes.pdf").to_vec();
        let doc = Document::open(bytes).unwrap();
        assert_eq!(doc.page_count().unwrap(), 3);
        assert_eq!(doc.page(0).unwrap().media_box, [0.0, 0.0, 612.0, 792.0]);
        assert_eq!(doc.page(1).unwrap().media_box, [0.0, 0.0, 841.0, 595.0]);
        assert_eq!(doc.page(2).unwrap().media_box, [0.0, 0.0, 300.0, 300.0]);
    }

    /// PDF avec un `/Length` de flux de contenu délibérément trop court
    /// (erreur d'auteurs réelle courante), différente de la corruption déjà
    /// couverte (xref tronquée) : le parseur doit retrouver la fin réelle du
    /// flux via `endstream` plutôt que de tronquer silencieusement le
    /// contenu à la valeur (fausse) de `/Length`.
    #[test]
    fn recovers_full_stream_content_despite_a_wrong_length_entry() {
        let bytes = include_bytes!("../tests/fixtures/malformed_wrong_length.pdf").to_vec();
        let doc = Document::open(bytes).unwrap();
        let page = doc.page(0).unwrap();
        let content = doc.page_content(&page).unwrap();
        let text = String::from_utf8_lossy(&content);
        assert!(
            text.contains("ET"),
            "expected the full content stream (including its closing ET) to be recovered, got: {text}"
        );
    }

    /// `/ColorSpace /Indexed` sur une image n'est pas supporté
    /// (`image.rs::resolve_color_space`) : ouvrir le document et interpréter
    /// la page ne doit pas planter, et l'image doit apparaître dans la
    /// `DisplayList` avec `pixels: None` (dégradation gracieuse déjà
    /// documentée) plutôt que de faire échouer toute la page.
    #[test]
    fn indexed_color_space_image_degrades_gracefully_instead_of_crashing() {
        let bytes = include_bytes!("../tests/fixtures/indexed_color_image.pdf").to_vec();
        let doc = Document::open(bytes).unwrap();
        let page = doc.page(0).unwrap();
        let content = doc.page_content(&page).unwrap();
        let display =
            crate::interp::Interpreter::run_page(&doc, page.resources.clone(), &content).unwrap();
        let images: Vec<&crate::display::DisplayItem> = display
            .items
            .iter()
            .filter(|i| matches!(i, crate::display::DisplayItem::Image { .. }))
            .collect();
        assert_eq!(images.len(), 1);
        let crate::display::DisplayItem::Image { pixels, .. } = images[0] else {
            unreachable!()
        };
        assert!(
            pixels.is_none(),
            "Indexed color space is not supported yet — expected no decoded pixels"
        );
    }

    #[test]
    fn opens_pdf_with_embedded_cjk_font_without_crashing() {
        // Texte CJK dessiné avec une police TrueType embarquée (Songti) : le
        // pipeline ne doit pas paniquer même si la résolution Unicode via
        // `/Encoding` (pensée pour WinAnsi/StandardEncoding) ne couvre pas
        // ces codes — voir STATUS.md pour la limite documentée (glyphes
        // dessinés via le contour, mais 0 caractère Unicode récupéré).
        let bytes = include_bytes!("../tests/fixtures/cjk_text.pdf").to_vec();
        let doc = Document::open(bytes).unwrap();
        assert_eq!(doc.page_count().unwrap(), 1);
    }

    #[test]
    fn opens_large_multipage_pdf() {
        let bytes = include_bytes!("../tests/fixtures/large_60_pages.pdf").to_vec();
        let doc = Document::open(bytes).unwrap();
        assert_eq!(doc.page_count().unwrap(), 60);
        let last_page = doc.page(59).unwrap();
        assert_eq!(last_page.media_box, [0.0, 0.0, 612.0, 792.0]);
    }
}
