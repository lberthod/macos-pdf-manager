//! Arbre des pages (`/Pages` -> `/Page`) avec attributs hérités —
//! architecture.md §4.4. Remplace le raccourci `/Count` utilisé jusqu'ici
//! (Sprint 3-4) par un parcours réel de l'arbre, seul moyen fiable de
//! retrouver les ressources/MediaBox/Rotate applicables à chaque page (ces
//! attributs peuvent être hérités depuis un nœud `/Pages` parent).

use crate::document::Document;
use crate::error::{PdfError, Result};
use crate::object::{Dictionary, ObjRef, Object};
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
    /// Référence indirecte de l'objet page lui-même, quand `/Kids` la
    /// désignait par référence (cas normal) plutôt que par un dictionnaire
    /// direct inline. Sert à résoudre les destinations `/Outlines`/`/Dest`
    /// (qui pointent vers l'objet page) en index de page — voir `outline.rs`.
    pub object_ref: Option<ObjRef>,
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
        let object_ref = node_obj.as_reference();

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
            object_ref,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Construit un PDF minimal à partir d'un corps d'objets déjà formaté
    /// (`"N 0 obj\n...\nendobj\n"` par objet) — même technique que les tests
    /// de `document.rs`/`redact.rs`, calcule les offsets réels plutôt que de
    /// les coder en dur.
    fn build_pdf(objects: &[String]) -> Vec<u8> {
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::with_capacity(objects.len());
        for obj in objects {
            offsets.push(bytes.len());
            bytes.extend_from_slice(obj.as_bytes());
        }
        let xref_offset = bytes.len();
        let mut xref = format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
        for off in &offsets {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        xref.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF",
            objects.len() + 1
        ));
        bytes.extend_from_slice(xref.as_bytes());
        bytes
    }

    fn obj(num: u32, body: &str) -> String {
        format!("{num} 0 obj\n{body}\nendobj\n")
    }

    /// AUDIT_FIXES_2026-09-03.md #19 : héritage de `/Resources`, `/MediaBox`
    /// et `/Rotate` sur trois niveaux — racine `/Pages` (1) -> nœud
    /// intermédiaire `/Pages` (2, redéfinit `/Rotate`) -> page feuille (3,
    /// redéfinit `/Resources`) ; `/MediaBox` doit remonter jusqu'à la racine.
    #[test]
    fn inherits_resources_mediabox_and_rotate_across_three_levels() {
        let objects = vec![
            obj(1, "<< /Type /Catalog /Pages 2 0 R >>"),
            obj(
                2,
                "<< /Type /Pages /Kids [3 0 R] /Count 1 \
                 /MediaBox [0 0 200 300] /Resources << /Font << /Base /F0 >> >> >>",
            ),
            obj(3, "<< /Type /Pages /Kids [4 0 R] /Count 1 /Rotate 90 >>"),
            obj(
                4,
                "<< /Type /Page /Parent 3 0 R \
                 /Resources << /Font << /Leaf /F1 >> >> >>",
            ),
        ];
        let doc = Document::open(build_pdf(&objects)).unwrap();
        let pages = doc.pages().unwrap();
        assert_eq!(pages.len(), 1);
        let page = &pages[0];

        // MediaBox : hérité depuis la racine (jamais redéfini plus bas).
        assert_eq!(page.media_box, [0.0, 0.0, 200.0, 300.0]);
        // Rotate : hérité du nœud intermédiaire (2), pas de la racine (0).
        assert_eq!(page.rotate, 90);
        // Resources : celles de la feuille elle-même priment sur celles,
        // différentes, du nœud racine.
        assert_eq!(
            page.resources
                .get("Font")
                .and_then(|o| o.as_dict())
                .and_then(|d| d.get("Leaf")),
            Some(&Object::Name("F1".to_string()))
        );
    }

    /// Sans redéfinition locale, une page doit recevoir `/Resources` du
    /// nœud `/Pages` parent, pas un dictionnaire vide.
    #[test]
    fn inherits_resources_from_parent_when_leaf_has_none() {
        let objects = vec![
            obj(1, "<< /Type /Catalog /Pages 2 0 R >>"),
            obj(
                2,
                "<< /Type /Pages /Kids [3 0 R] /Count 1 \
                 /MediaBox [0 0 612 792] /Resources << /Font << /F0 /F0 >> >> >>",
            ),
            obj(3, "<< /Type /Page /Parent 2 0 R >>"),
        ];
        let doc = Document::open(build_pdf(&objects)).unwrap();
        let page = doc.page(0).unwrap();
        assert!(page.resources.get("Font").is_some());
    }

    /// AUDIT_FIXES_2026-09-03.md #19 : un cycle dans `/Kids` (le nœud 2 se
    /// référence lui-même) ne doit jamais boucler indéfiniment — la garde
    /// `visited` de `collect_pages` doit l'arrêter silencieusement plutôt
    /// que de faire un déni de service ou un stack overflow.
    #[test]
    fn kids_cycle_does_not_loop_forever() {
        let objects = vec![
            obj(1, "<< /Type /Catalog /Pages 2 0 R >>"),
            // Le nœud 2 se contient lui-même dans ses propres /Kids, en plus
            // d'une vraie page (3) — la garde de cycle doit court-circuiter
            // la branche récursive sans empêcher de trouver la page réelle.
            obj(
                2,
                "<< /Type /Pages /Kids [2 0 R 3 0 R] /Count 2 /MediaBox [0 0 612 792] >>",
            ),
            obj(3, "<< /Type /Page /Parent 2 0 R >>"),
        ];
        let doc = Document::open(build_pdf(&objects)).unwrap();
        // Ne doit ni boucler ni paniquer, et doit tout de même trouver la
        // page réelle atteignable une fois le cycle court-circuité.
        let pages = doc.pages().unwrap();
        assert_eq!(pages.len(), 1);
    }

    /// `/Count` incohérent (annonce 5 pages, `/Kids` n'en a qu'une) ne doit
    /// pas être pris pour argent comptant — `pages()`/`page_count()` fait un
    /// vrai parcours de `/Kids`, jamais confiance en `/Count` seul (voir la
    /// doc de module).
    #[test]
    fn ignores_an_inconsistent_count_and_trusts_the_real_kids_traversal() {
        let objects = vec![
            obj(1, "<< /Type /Catalog /Pages 2 0 R >>"),
            obj(
                2,
                "<< /Type /Pages /Kids [3 0 R] /Count 5 /MediaBox [0 0 612 792] >>",
            ),
            obj(3, "<< /Type /Page /Parent 2 0 R >>"),
        ];
        let doc = Document::open(build_pdf(&objects)).unwrap();
        assert_eq!(doc.page_count().unwrap(), 1);
    }
}
