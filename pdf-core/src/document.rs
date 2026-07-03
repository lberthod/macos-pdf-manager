//! Modèle document (arbre logique) — architecture.md §4.4.

use crate::error::{PdfError, Result};
use crate::object::{Dictionary, ObjRef, Object};
use crate::parser::Parser;
use crate::xref::{parse_xref_chain, XrefTable};
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
        Ok(Self {
            data,
            xref,
            trailer,
            cache: RefCell::new(BTreeMap::new()),
        })
    }

    pub fn object_count(&self) -> usize {
        self.xref.offsets.len()
    }

    pub fn trailer(&self) -> &Dictionary {
        &self.trailer
    }

    /// Résout un objet indirect par numéro (la génération n'est pas encore
    /// vérifiée : suffisant tant que les PDF avec objets libérés/réutilisés
    /// ne sont pas dans le corpus de test prioritaire).
    pub fn resolve(&self, r: ObjRef) -> Result<Object> {
        if let Some(cached) = self.cache.borrow().get(&r.num) {
            return Ok(cached.clone());
        }
        let offset = *self
            .xref
            .offsets
            .get(&r.num)
            .ok_or(PdfError::ObjectNotFound(r.num, r.gen))?;
        let mut parser = Parser::with_pos(&self.data, offset);
        let (_num, _gen, object) = parser.parse_indirect_object()?;
        self.cache.borrow_mut().insert(r.num, object.clone());
        Ok(object)
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

    fn pages_root(&self) -> Result<Dictionary> {
        let root = self.root()?;
        let pages_obj = root
            .get("Pages")
            .ok_or_else(|| PdfError::MissingKey("Pages".into()))?;
        let pages = self.get(pages_obj)?;
        pages
            .as_dict()
            .cloned()
            .ok_or(PdfError::UnexpectedType("Dictionary"))
    }

    /// Nombre de pages via `/Root /Pages /Count` (ne parcourt pas l'arbre :
    /// suffisant pour la majorité des PDF bien formés ; le parcours complet
    /// de l'arbre des pages est prévu Sprint 5-6, voir sprint.md).
    pub fn page_count(&self) -> Result<usize> {
        let pages = self.pages_root()?;
        pages.get_int("Count").map(|n| n as usize)
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
}
