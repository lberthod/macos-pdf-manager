//! Modèle document (arbre logique) — architecture.md §4.4.

use crate::crypt::{DecryptError, Decryptor};
use crate::error::{PdfError, Result};
use crate::filters::decode_stream;
use crate::object::{Dictionary, ObjRef, Object};
use crate::parser::Parser;
use crate::writer::{write_indirect_object, write_object};
use crate::xref::{find_startxref_offset, parse_xref_chain, XrefEntry, XrefTable};
use std::cell::RefCell;
use std::collections::BTreeMap;

pub struct Document {
    data: Vec<u8>,
    xref: XrefTable,
    trailer: Dictionary,
    cache: RefCell<BTreeMap<u32, Object>>,
    /// Contexte de déchiffrement (`/Encrypt`, Sprint 22/58, voir `crypt.rs`)
    /// — `None` pour un document non chiffré.
    decryption: Option<Decryptor>,
}

impl Document {
    /// Ouvre un document, en supposant un mot de passe utilisateur vide (le
    /// cas normal — la grande majorité des PDF "protégés" ne restreignent
    /// que l'édition/l'impression, pas l'ouverture). Pour un document dont
    /// le mot de passe utilisateur est réellement non vide, utiliser
    /// `open_with_password` — celui-ci renvoie `PdfError::IncorrectPassword`
    /// plutôt qu'un succès trompeur (voir la doc de module de `crypt` pour
    /// le bug que ça corrigeait avant le Sprint 58, `audit50quest.md` #50).
    pub fn open(data: Vec<u8>) -> Result<Self> {
        Self::open_with_password(data, b"")
    }

    /// Comme `open`, avec un mot de passe utilisateur explicite (octets
    /// bruts — encodage PDFDocEncoding/UTF-8 selon la révision, ISO
    /// 32000-1 §7.6.4.3.2 ; en pratique un mot de passe ASCII simple couvre
    /// la quasi-totalité des cas réels, pas de conversion d'encodage faite
    /// ici). Sans effet sur un document non chiffré.
    pub fn open_with_password(data: Vec<u8>, password: &[u8]) -> Result<Self> {
        let (xref, trailer) = parse_xref_chain(&data)?;
        let mut doc = Self {
            data,
            xref,
            trailer,
            cache: RefCell::new(BTreeMap::new()),
            decryption: None,
        };

        if let Some(encrypt_obj) = doc.trailer.get("Encrypt").cloned() {
            // Résolu avant que `decryption` ne soit renseigné : correct,
            // le dictionnaire `/Encrypt` lui-même n'est jamais chiffré.
            let encrypt_dict = doc
                .get(&encrypt_obj)?
                .as_dict()
                .cloned()
                .ok_or(PdfError::UnexpectedType("Dictionary"))?;
            let encrypt_obj_num = encrypt_obj.as_reference().map(|r| r.num);
            let id0 = doc
                .trailer
                .get("ID")
                .and_then(|o| o.as_array())
                .and_then(|a| a.first())
                .and_then(|o| match o {
                    Object::String(bytes) => Some(bytes.as_slice()),
                    _ => None,
                })
                .unwrap_or(&[]);
            match Decryptor::new(&encrypt_dict, id0, encrypt_obj_num, password) {
                Ok(decryptor) => doc.decryption = Some(decryptor),
                // Gestionnaire de sécurité non standard, ou révision non
                // reconnue : on ne peut pas déchiffrer, message clair plutôt
                // que des erreurs de bas niveau trompeuses plus loin.
                Err(DecryptError::Unsupported) => return Err(PdfError::Encrypted),
                // Mot de passe incorrect (ou vide sur un document qui en
                // exige un réel) : distinct de `Encrypted` pour que
                // l'appelant (`pdf-app`/`pdf-ui`) sache qu'il peut proposer
                // de ressaisir un mot de passe plutôt qu'abandonner.
                Err(DecryptError::WrongPassword) => return Err(PdfError::IncorrectPassword),
            }
        }

        Ok(doc)
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

        let mut object = match entry {
            XrefEntry::Offset(offset) => {
                let mut parser = Parser::with_pos(&self.data, offset);
                let (_num, _gen, object) = parser.parse_indirect_object()?;
                object
            }
            XrefEntry::Compressed { stream_num, index } => {
                self.resolve_compressed(stream_num, index)?
            }
        };

        // Déchiffrement (Sprint 22, `crypt.rs`) : seulement pour les objets à
        // offset direct — un objet compressé dans un `/ObjStm` a déjà été
        // déchiffré en tant que flux quand ce dernier a lui-même été résolu
        // (voir `resolve_compressed`), il ne doit pas l'être une seconde
        // fois. Le dictionnaire `/Encrypt` lui-même n'est jamais déchiffré.
        if let Some(decryptor) = &self.decryption {
            if matches!(entry, XrefEntry::Offset(_)) && Some(r.num) != decryptor.encrypt_obj_num() {
                decryptor.decrypt_object(&mut object, r.num, r.gen);
            }
        }

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

    /// Plus petit numéro d'objet libre (Sprint 13-14, `pdf-edit`) : plus
    /// grand numéro déjà utilisé dans la table xref courante, +1. Ne
    /// consulte pas `/Size` du trailer (qui peut être incohérent sur un PDF
    /// malformé) — la table xref réellement résolue fait foi.
    pub fn next_free_object_num(&self) -> u32 {
        self.xref.entries.keys().copied().max().unwrap_or(0) + 1
    }

    /// Sauvegarde incrémentale (ISO 32000-1 §7.5.6, Sprint 13-14) : ajoute
    /// `objects` (nouveaux objets **ou** mises à jour d'objets existants) à
    /// la fin du fichier original, suivis d'une nouvelle section xref
    /// classique et d'un nouveau `trailer` chaîné par `/Prev` à l'ancien
    /// `startxref` — le fichier d'origine n'est jamais modifié en place,
    /// seulement complété (`append`), ce qui rend l'opération sûre même en
    /// cas d'échec à mi-chemin (le fichier original reste un PDF valide en
    /// préfixe). Retourne les octets complets du nouveau fichier ; c'est
    /// l'appelant qui les écrit sur disque.
    pub fn save_incremental(&self, objects: &[(ObjRef, Object)]) -> Result<Vec<u8>> {
        let prev_offset = find_startxref_offset(&self.data)?;
        let root_obj = self
            .trailer
            .get("Root")
            .cloned()
            .ok_or_else(|| PdfError::MissingKey("Root".into()))?;

        let mut out = self.data.clone();
        if !out.ends_with(b"\n") {
            out.push(b'\n');
        }

        let mut offsets: Vec<(u32, usize)> = Vec::with_capacity(objects.len());
        for (r, obj) in objects {
            offsets.push((r.num, out.len()));
            write_indirect_object(r.num, r.gen, obj, &mut out);
        }
        offsets.sort_by_key(|(num, _)| *num);

        let xref_offset = out.len();
        out.extend_from_slice(b"xref\n");
        let mut i = 0;
        while i < offsets.len() {
            let mut j = i;
            while j + 1 < offsets.len() && offsets[j + 1].0 == offsets[j].0 + 1 {
                j += 1;
            }
            let start_num = offsets[i].0;
            let count = j - i + 1;
            out.extend_from_slice(format!("{start_num} {count}\n").as_bytes());
            for entry in &offsets[i..=j] {
                // Format ISO 32000-1 §7.5.4 : 10 chiffres, espace, 5
                // chiffres, espace, 'n', espace, fin de ligne (20 octets) —
                // notre propre parseur ne l'exige pas (il tokenise plutôt
                // que de lire à largeur fixe) mais d'autres lecteurs si.
                out.extend_from_slice(format!("{:010} 00000 n \n", entry.1).as_bytes());
            }
            i = j + 1;
        }

        let max_existing = self.xref.entries.keys().copied().max().unwrap_or(0);
        let max_new = offsets.last().map(|(num, _)| *num).unwrap_or(0);
        let size = max_existing.max(max_new) + 1;

        let mut trailer_dict = Dictionary::new();
        trailer_dict.insert("Size", Object::Integer(size as i64));
        trailer_dict.insert("Root", root_obj);
        trailer_dict.insert("Prev", Object::Integer(prev_offset as i64));

        out.extend_from_slice(b"trailer\n");
        write_object(&Object::Dictionary(trailer_dict), &mut out);
        out.extend_from_slice(format!("\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());

        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Concatène les glyphes résolus en Unicode d'une `DisplayList`, dans
    /// l'ordre d'émission — même technique que `pdf-cli render-info`
    /// ("Recovered text"), en local pour ne pas faire dépendre `pdf-core`
    /// de `pdf-text` (qui dépend déjà de `pdf-core`, pas l'inverse).
    fn recovered_text_for_test(display: &crate::display::DisplayList) -> String {
        display
            .items
            .iter()
            .filter_map(|item| match item {
                crate::display::DisplayItem::Glyph { unicode, .. } => *unicode,
                _ => None,
            })
            .collect()
    }

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

    /// Sprint 13-14 : sauvegarde incrémentale — ajoute un nouvel objet (une
    /// "annotation" simplifiée à un seul entier, le contenu réel importe peu
    /// ici) et met à jour la page existante pour y pointer, puis vérifie
    /// qu'une réouverture complète du fichier obtenu retrouve les deux :
    /// l'objet existant modifié **et** le nouvel objet, sans avoir touché
    /// à un seul octet du fichier original (préfixe inchangé).
    #[test]
    fn save_incremental_appends_new_and_updated_objects_readable_after_reopen() {
        let original = minimal_pdf();
        let doc = Document::open(original.clone()).unwrap();
        let next_num = doc.next_free_object_num();
        assert_eq!(next_num, 4, "objects 1..3 already used by minimal_pdf()");

        // Nouvel objet, numéro jamais vu dans le fichier original.
        let new_marker = Object::Integer(999);
        let new_ref = ObjRef::new(next_num, 0);

        // Objet existant (la page, numéro 3) mis à jour pour référencer le
        // nouvel objet — simule ce que ferait une vraie annotation ajoutée
        // à `/Annots`.
        let mut updated_page = doc
            .resolve(ObjRef::new(3, 0))
            .unwrap()
            .as_dict()
            .unwrap()
            .clone();
        updated_page.insert("Marker", Object::Reference(new_ref));

        let new_bytes = doc
            .save_incremental(&[
                (new_ref, new_marker),
                (ObjRef::new(3, 0), Object::Dictionary(updated_page)),
            ])
            .unwrap();

        assert!(
            new_bytes.starts_with(&original),
            "incremental save must never rewrite the original file bytes, only append"
        );

        let reopened = Document::open(new_bytes).unwrap();
        assert_eq!(reopened.page_count().unwrap(), 1);

        let page = reopened.page(0).unwrap();
        let marker_ref = page.dict.get("Marker").unwrap().as_reference().unwrap();
        assert_eq!(marker_ref, new_ref);
        let marker_value = reopened.resolve(marker_ref).unwrap();
        assert_eq!(marker_value.as_int(), Some(999));
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
    fn opens_and_decrypts_an_aes128_encrypted_pdf_with_empty_user_password() {
        // Malgré son nom (`pdf-core/tests/fixtures/README.md` documentait
        // l'intention "RC4 40 bits", voir sa note de correction), ce fixture
        // est en réalité chiffré `/V 4 /R 4` avec un filtre de chiffrement
        // `/CFM /AESV2` (AES-128-CBC) — pikepdf choisit AES par défaut pour
        // `R=4` sans `aes=False` explicite. Sert malgré tout de test de
        // bout en bout pour la dérivation de clé Algorithme 2 (ISO 32000-1
        // §7.6.3.3) + AES-128, avec mot de passe utilisateur vide.
        let bytes = include_bytes!("../tests/fixtures/encrypted_rc4.pdf").to_vec();
        let doc = Document::open(bytes).expect("should decrypt with the empty user password");
        let page = doc.page(0).unwrap();
        let content = doc.page_content(&page).unwrap();
        let display =
            crate::interp::Interpreter::run_page(&doc, page.resources.clone(), &content).unwrap();
        let text = recovered_text_for_test(&display);
        assert_eq!(
            text, "Encrypted PDF test - if you can read this, decryption worked",
            "decrypted content stream must produce the original plaintext"
        );
    }

    /// Symétrique pour la révision 6 (AES-256, "hardened hash" ISO 32000-2
    /// Annexe C) plutôt que la révision 4 (AES-128) déjà couverte : les deux
    /// chemins de dérivation de clé (Algorithme 2 classique vs Algorithme
    /// 2.A/2.B) doivent produire un déchiffrement correct, pas seulement le
    /// premier rencontré.
    #[test]
    fn opens_and_decrypts_an_aes256_r6_encrypted_pdf_with_empty_user_password() {
        let bytes = include_bytes!("../tests/fixtures/encrypted_aes256.pdf").to_vec();
        let doc = Document::open(bytes).expect("should decrypt with the empty user password");
        let page = doc.page(0).unwrap();
        let content = doc.page_content(&page).unwrap();
        let display =
            crate::interp::Interpreter::run_page(&doc, page.resources.clone(), &content).unwrap();
        let text = recovered_text_for_test(&display);
        assert_eq!(
            text, "AES-256 encrypted PDF test",
            "decrypted content stream must produce the original plaintext"
        );
    }

    /// Sprint 58 (`audit50quest.md` #50) : un mot de passe utilisateur réel
    /// (pas vide) fonctionne bout en bout via `open_with_password`, et
    /// `Document::open` (mot de passe vide implicite) échoue proprement sur
    /// le même fichier avec `PdfError::IncorrectPassword` plutôt qu'un
    /// contenu déchiffré silencieusement corrompu (le bug documenté dans
    /// `crypt.rs` avant cette passe).
    #[test]
    fn opens_an_aes128_encrypted_pdf_with_a_real_non_empty_user_password() {
        let bytes = include_bytes!("../tests/fixtures/encrypted_user_password.pdf").to_vec();

        let doc = Document::open_with_password(bytes.clone(), b"secret123")
            .expect("should decrypt with the correct non-empty user password");
        let page = doc.page(0).unwrap();
        let content = doc.page_content(&page).unwrap();
        let display =
            crate::interp::Interpreter::run_page(&doc, page.resources.clone(), &content).unwrap();
        let text = recovered_text_for_test(&display);
        assert_eq!(
            text,
            "Password protected PDF test - if you can read this, the password worked"
        );

        let wrong = Document::open_with_password(bytes.clone(), b"wrong-password");
        assert!(matches!(wrong, Err(PdfError::IncorrectPassword)));

        let empty = Document::open(bytes);
        assert!(matches!(empty, Err(PdfError::IncorrectPassword)));
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
