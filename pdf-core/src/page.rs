//! Arbre des pages (`/Pages` -> `/Page`) avec attributs hérités —
//! architecture.md §4.4. Remplace le raccourci `/Count` utilisé jusqu'ici
//! (Sprint 3-4) par un parcours réel de l'arbre, seul moyen fiable de
//! retrouver les ressources/MediaBox/Rotate applicables à chaque page (ces
//! attributs peuvent être hérités depuis un nœud `/Pages` parent).

use crate::document::Document;
use crate::error::{PdfError, Result};
use crate::object::{Dictionary, Object};
use std::collections::HashSet;

/// Une page « feuille » de l'arbre, avec ses attributs déjà résolus
/// (héritage inclus).
#[derive(Debug, Clone)]
pub struct Page {
    pub index: usize,
    pub dict: Dictionary,
    pub media_box: [f64; 4],
    pub rotate: i32,
    pub resources: Dictionary,
}

#[derive(Debug, Clone, Default)]
struct Inherited {
    resources: Option<Object>,
    media_box: Option<Object>,
    rotate: Option<i64>,
}

impl Document {
    /// Parcourt récursivement `/Root /Pages` et retourne la liste des pages
    /// dans l'ordre du document. Protège contre les cycles `/Kids`
    /// malformés (référence vers un ancêtre).
    pub fn pages(&self) -> Result<Vec<Page>> {
        let root = self.root()?;
        let pages_ref = root
            .get("Pages")
            .ok_or_else(|| PdfError::MissingKey("Pages".into()))?
            .clone();

        let mut out = Vec::new();
        let mut visited = HashSet::new();
        self.collect_pages(&pages_ref, &Inherited::default(), &mut out, &mut visited)?;
        Ok(out)
    }

    /// Retourne la N-ième page (0-indexée). Simple confort au-dessus de
    /// `pages()` ; reparcourt l'arbre à chaque appel (acceptable pour des
    /// documents de taille modeste — un cache pourra être ajouté plus tard
    /// si nécessaire).
    pub fn page(&self, index: usize) -> Result<Page> {
        self.pages()?
            .into_iter()
            .nth(index)
            .ok_or_else(|| PdfError::InvalidObject(0, format!("no page at index {index}")))
    }

    fn collect_pages(
        &self,
        node_obj: &Object,
        inherited: &Inherited,
        out: &mut Vec<Page>,
        visited: &mut HashSet<(u32, u16)>,
    ) -> Result<()> {
        if let Object::Reference(r) = node_obj {
            if !visited.insert((r.num, r.gen)) {
                return Ok(()); // cycle détecté : on ignore silencieusement.
            }
        }

        let node = self.get(node_obj)?;
        let Some(dict) = node.as_dict() else {
            return Ok(()); // nœud malformé : on l'ignore plutôt que d'échouer tout le document.
        };
        let dict = dict.clone();

        let mut inherited = inherited.clone();
        if let Some(res) = dict.get("Resources") {
            inherited.resources = Some(res.clone());
        }
        if let Some(mb) = dict.get("MediaBox") {
            inherited.media_box = Some(mb.clone());
        }
        if let Some(rotate) = dict.get("Rotate").and_then(|o| o.as_int()) {
            inherited.rotate = Some(rotate);
        }

        if dict.get("Type").and_then(|o| o.as_name()) == Some("Pages") {
            let kids_obj = dict.get("Kids").cloned().unwrap_or(Object::Array(vec![]));
            let kids = self.get(&kids_obj)?;
            for kid in kids.as_array().unwrap_or(&[]) {
                self.collect_pages(kid, &inherited, out, visited)?;
            }
            return Ok(());
        }

        // Traité comme `/Page` (certains PDF omettent `/Type` sur les feuilles).
        let media_box = inherited
            .media_box
            .as_ref()
            .and_then(|o| self.resolve_rect(o).ok())
            .unwrap_or([0.0, 0.0, 612.0, 792.0]);

        let resources = match &inherited.resources {
            Some(obj) => self
                .get(obj)?
                .as_dict()
                .cloned()
                .unwrap_or_else(Dictionary::new),
            None => Dictionary::new(),
        };

        out.push(Page {
            index: out.len(),
            dict,
            media_box,
            rotate: inherited.rotate.unwrap_or(0) as i32,
            resources,
        });
        Ok(())
    }

    fn resolve_rect(&self, obj: &Object) -> Result<[f64; 4]> {
        let resolved = self.get(obj)?;
        let items = resolved
            .as_array()
            .ok_or(PdfError::UnexpectedType("Array"))?;
        if items.len() != 4 {
            return Err(PdfError::UnexpectedType("Array of 4 numbers"));
        }
        let mut out = [0.0; 4];
        for (i, item) in items.iter().enumerate() {
            out[i] = item
                .as_int()
                .map(|n| n as f64)
                .unwrap_or_else(|| match item {
                    Object::Real(f) => *f,
                    _ => 0.0,
                });
        }
        Ok(out)
    }

    /// Concatène les flux `/Contents` d'une page (un seul stream ou un
    /// tableau de streams, ISO 32000-1 §7.8.2) et les décode.
    pub fn page_content(&self, page: &Page) -> Result<Vec<u8>> {
        let Some(contents_obj) = page.dict.get("Contents") else {
            return Ok(Vec::new());
        };
        let contents = self.get(contents_obj)?;

        let streams: Vec<Object> = match &contents {
            Object::Stream(_) => vec![contents.clone()],
            Object::Array(_) => contents_obj
                .as_array()
                .unwrap_or(&[])
                .iter()
                .map(|o| self.get(o))
                .collect::<Result<Vec<_>>>()?,
            _ => return Err(PdfError::UnexpectedType("Stream or Array of Stream")),
        };

        let mut out = Vec::new();
        for obj in streams {
            let Object::Stream(stream) = obj else {
                continue;
            };
            out.extend_from_slice(&crate::filters::decode_stream(&stream)?);
            out.push(b'\n'); // sépare les flux concaténés (ISO 32000-1 §7.8.2).
        }
        Ok(out)
    }
}
