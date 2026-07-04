//! Table des matières / signets (`/Root /Outlines`, ISO 32000-1 §12.3.3) —
//! Sprint 9-10 (panneau signets, voir sprint.md).
//!
//! Limitations connues :
//! - Seules les destinations directes (`/Dest` : tableau `[page /Fit ...]`
//!   pointant sur l'objet page par référence) sont résolues en index de
//!   page. Les destinations nommées (`/Dest` nom + arbre `/Names/Dests`, ou
//!   ancien style `/Root /Dests`) et les actions `/A` (`/GoTo`, JavaScript,
//!   etc.) ne sont pas gérées : l'entrée est alors gardée avec
//!   `page_index: None` plutôt que d'être supprimée.
//! - Titres décodés via `Object::as_text_string` (PDFDocEncoding approximé
//!   par UTF-8 lossy, UTF-16BE avec BOM géré) — voir sa documentation.

use std::collections::HashSet;

use crate::document::Document;
use crate::error::Result;
use crate::object::{ObjRef, Object};
use crate::page::Page;

/// Une entrée de la table des matières, avec ses éventuels enfants
/// (structure arborescente, un niveau par indentation dans le panneau UI).
#[derive(Debug, Clone, PartialEq)]
pub struct OutlineItem {
    pub title: String,
    /// Page cible (0-based), si la destination a pu être résolue — voir les
    /// limitations en tête de module.
    pub page_index: Option<usize>,
    pub children: Vec<OutlineItem>,
}

impl Document {
    /// Lit la table des matières du document ; `[]` si absente ou vide
    /// (cas normal pour la plupart des PDF).
    pub fn outline(&self) -> Result<Vec<OutlineItem>> {
        let root = self.root()?;
        let Some(outlines_obj) = root.get("Outlines") else {
            return Ok(Vec::new());
        };
        let Some(outlines_dict) = self.get(outlines_obj)?.as_dict().cloned() else {
            return Ok(Vec::new());
        };
        let Some(first) = outlines_dict.get("First").cloned() else {
            return Ok(Vec::new());
        };

        let pages = self.pages()?;
        let mut visited = HashSet::new();
        self.collect_outline_siblings(&first, &pages, &mut visited)
    }

    /// Parcourt une chaîne `/Next` (les enfants d'un même parent) en
    /// résolvant récursivement les sous-arbres via `/First`.
    fn collect_outline_siblings(
        &self,
        first_obj: &Object,
        pages: &[Page],
        visited: &mut HashSet<(u32, u16)>,
    ) -> Result<Vec<OutlineItem>> {
        let mut items = Vec::new();
        let mut current = first_obj.clone();

        loop {
            if let Object::Reference(r) = &current {
                if !visited.insert((r.num, r.gen)) {
                    break; // `/Next` cyclique malformé : on s'arrête plutôt que boucler.
                }
            }
            let Some(dict) = self.get(&current)?.as_dict().cloned() else {
                break;
            };

            let title = dict
                .get("Title")
                .and_then(|o| o.as_text_string())
                .unwrap_or_default();
            let page_index = dict
                .get("Dest")
                .and_then(|o| o.as_array())
                .and_then(|arr| arr.first())
                .and_then(|target| resolve_page_index(target, pages));
            let children = match dict.get("First") {
                Some(first_child) => self.collect_outline_siblings(first_child, pages, visited)?,
                None => Vec::new(),
            };

            items.push(OutlineItem {
                title,
                page_index,
                children,
            });

            match dict.get("Next") {
                Some(next) => current = next.clone(),
                None => break,
            }
        }

        Ok(items)
    }
}

fn resolve_page_index(target: &Object, pages: &[Page]) -> Option<usize> {
    let ObjRef { num, gen } = target.as_reference()?;
    pages
        .iter()
        .find(|p| p.object_ref == Some(ObjRef::new(num, gen)))
        .map(|p| p.index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::Dictionary;

    #[test]
    fn no_outlines_returns_empty_vec() {
        let bytes = include_bytes!("../tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let doc = Document::open(bytes).unwrap();
        assert_eq!(doc.outline().unwrap(), Vec::new());
    }

    #[test]
    fn real_fixture_reads_flat_outline_with_resolved_page_indices() {
        let bytes = include_bytes!("../tests/fixtures/outline_test.pdf").to_vec();
        let doc = Document::open(bytes).unwrap();
        let outline = doc.outline().unwrap();

        assert_eq!(outline.len(), 4);
        for (i, item) in outline.iter().enumerate() {
            assert_eq!(item.title, format!("Section {}", i + 1));
            assert_eq!(item.page_index, Some(i));
            assert!(item.children.is_empty());
        }
    }

    #[test]
    fn nested_outline_children_are_resolved() {
        // Construit une table des matières à la main : "Chapitre 1" (page 0)
        // avec un enfant "Section 1.1" (page 1), pour vérifier `/First`
        // imbriqué sans dépendre d'un fixture externe supplémentaire.
        let bytes = include_bytes!("../tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let doc = Document::open(bytes).unwrap();
        let pages = doc.pages().unwrap();
        let page0_ref = pages[0].object_ref.unwrap();
        let page1_ref = pages[1].object_ref.unwrap();

        let mut child = Dictionary::new();
        child.insert("Title", Object::String(b"Section 1.1".to_vec()));
        child.insert(
            "Dest",
            Object::Array(vec![
                Object::Reference(page1_ref),
                Object::Name("Fit".into()),
            ]),
        );

        let mut parent = Dictionary::new();
        parent.insert("Title", Object::String(b"Chapitre 1".to_vec()));
        parent.insert(
            "Dest",
            Object::Array(vec![
                Object::Reference(page0_ref),
                Object::Name("Fit".into()),
            ]),
        );
        parent.insert("First", Object::Dictionary(child));

        let mut visited = HashSet::new();
        let items = doc
            .collect_outline_siblings(&Object::Dictionary(parent), &pages, &mut visited)
            .unwrap();

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Chapitre 1");
        assert_eq!(items[0].page_index, Some(0));
        assert_eq!(items[0].children.len(), 1);
        assert_eq!(items[0].children[0].title, "Section 1.1");
        assert_eq!(items[0].children[0].page_index, Some(1));
    }
}
