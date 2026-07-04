//! Extraction de texte à partir d'une `DisplayList` (pdf-core) —
//! architecture.md §4.6, préalable à la recherche texte du Sprint 9-10
//! (sprint.md).
//!
//! La résolution glyphe -> caractère Unicode reste entièrement à la charge
//! de `pdf-core::font` (`/Encoding`+`/Differences`, pas encore `/ToUnicode`
//! dédié ni polices composites `/Type0` — voir STATUS.md). Ce module ne fait
//! qu'assembler les caractères déjà résolus dans l'ordre d'émission de la
//! `DisplayList`, avec une heuristique de saut de ligne basée sur le
//! déplacement vertical de la ligne de base entre deux glyphes consécutifs
//! (pas de reconstruction de blocs/colonnes façon `pdftotext -layout`).

use pdf_core::display::{DisplayItem, DisplayList};

/// Fraction de la hauteur de police au-delà de laquelle un déplacement
/// vertical de la ligne de base est interprété comme un changement de ligne
/// plutôt qu'un exposant/indice ou du crénage fin.
const LINE_BREAK_THRESHOLD: f64 = 0.5;

/// Rectangle englobant d'un glyphe (ou d'une occurrence de recherche, voir
/// `PageText::find_matches`), en espace page PDF (origine bas-gauche, comme
/// `pdf_core::page::Page::media_box`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlyphRect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

/// Texte d'une page avec la position de chaque caractère, pour permettre le
/// surlignage des résultats de recherche (voir `find_matches`). Les
/// caractères de structure insérés (sauts de ligne) n'ont pas de position
/// (`None` dans `rects`, aligné caractère par caractère avec `text`).
pub struct PageText {
    text: String,
    rects: Vec<Option<GlyphRect>>,
}

impl PageText {
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Rectangles englobants (un par occurrence, fusion des rectangles de
    /// chaque caractère de l'occurrence) de `query` dans le texte de la
    /// page, comparaison insensible à la casse. La casse est repliée
    /// caractère par caractère via `to_ascii_lowercase` : les scripts non
    /// latins avec des règles de casse multi-caractères ne sont pas gérés
    /// correctement (limitation connue, acceptable pour un premier
    /// surlignage — voir STATUS.md).
    pub fn find_matches(&self, query: &str) -> Vec<GlyphRect> {
        if query.is_empty() {
            return Vec::new();
        }
        let haystack: Vec<char> = self.text.chars().map(|c| c.to_ascii_lowercase()).collect();
        let needle: Vec<char> = query.chars().map(|c| c.to_ascii_lowercase()).collect();
        if needle.len() > haystack.len() {
            return Vec::new();
        }

        let mut matches = Vec::new();
        for start in 0..=(haystack.len() - needle.len()) {
            if haystack[start..start + needle.len()] != needle[..] {
                continue;
            }
            let mut merged: Option<GlyphRect> = None;
            for rect in self.rects[start..start + needle.len()].iter().flatten() {
                merged = Some(match merged {
                    None => *rect,
                    Some(m) => GlyphRect {
                        x0: m.x0.min(rect.x0),
                        y0: m.y0.min(rect.y0),
                        x1: m.x1.max(rect.x1),
                        y1: m.y1.max(rect.y1),
                    },
                });
            }
            if let Some(rect) = merged {
                matches.push(rect);
            }
        }
        matches
    }
}

/// Comme `extract_text`, mais garde la position (espace page) de chaque
/// caractère pour permettre le surlignage (`PageText::find_matches`).
pub fn extract_page_text(display: &DisplayList) -> PageText {
    let mut text = String::new();
    let mut rects = Vec::new();
    let mut last_baseline: Option<(f64, f64)> = None; // (y de la ligne précédente, hauteur de police)

    for item in &display.items {
        let DisplayItem::Glyph {
            unicode: Some(c),
            transform,
            ..
        } = item
        else {
            continue;
        };

        // `transform` combine échelle de police + matrice texte + CTM ; sa
        // composante `d` approxime la taille de police en espace page (voir
        // `pdf_core::interp::Interpreter::show_text`), et `e`/`f` la
        // position du point d'origine (baseline) du glyphe.
        let x = transform.e;
        let y = transform.f;
        let font_height = transform.d.abs().max(1.0);

        if let Some((last_y, last_height)) = last_baseline {
            if (y - last_y).abs() > last_height.max(font_height) * LINE_BREAK_THRESHOLD {
                text.push('\n');
                rects.push(None);
            }
        }
        text.push(*c);
        // Largeur approximée (pas de largeur de glyphe exposée par
        // `DisplayItem::Glyph` — seul le contour l'est) : une fraction
        // raisonnable de la hauteur de police, suffisante pour un
        // rectangle de surlignage visuellement correct sans être exact au
        // pixel près.
        let width = font_height * 0.6;
        rects.push(Some(GlyphRect {
            x0: x,
            y0: y,
            x1: x + width,
            y1: y + font_height * 0.8,
        }));
        last_baseline = Some((y, font_height));
    }

    PageText { text, rects }
}

/// Concatène le texte d'une page dans l'ordre d'émission des glyphes de sa
/// `DisplayList`, en insérant un saut de ligne quand la ligne de base saute
/// verticalement de plus de la moitié de la taille de police courante. Les
/// glyphes sans résolution Unicode (police composite `/Type0`, encodage non
/// reconnu — `unicode: None`) sont silencieusement omis plutôt que
/// remplacés par un caractère de substitution.
pub fn extract_text(display: &DisplayList) -> String {
    extract_page_text(display).text
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdf_core::display::{Color, Matrix};

    fn glyph_at(c: char, x: f64, y: f64, font_size: f64) -> DisplayItem {
        DisplayItem::Glyph {
            font: "F1".into(),
            code: c as u32,
            unicode: Some(c),
            transform: Matrix::new(font_size, 0.0, 0.0, font_size, x, y),
            color: Color::default(),
            advance_is_estimated: false,
            outline: None,
            clip: None,
        }
    }

    #[test]
    fn concatenates_glyphs_on_the_same_baseline_without_break() {
        let display = DisplayList {
            items: vec![
                glyph_at('H', 0.0, 700.0, 12.0),
                glyph_at('i', 8.0, 700.0, 12.0),
            ],
        };
        assert_eq!(extract_text(&display), "Hi");
    }

    #[test]
    fn inserts_newline_on_large_vertical_baseline_jump() {
        let display = DisplayList {
            items: vec![
                glyph_at('A', 0.0, 700.0, 12.0),
                // Nouvelle ligne : la ligne de base descend de 20pt, bien
                // plus que la demi-hauteur de police (6pt).
                glyph_at('B', 0.0, 680.0, 12.0),
            ],
        };
        assert_eq!(extract_text(&display), "A\nB");
    }

    #[test]
    fn small_baseline_jitter_does_not_trigger_a_line_break() {
        // Un exposant ou un léger crénage vertical (< moitié de la taille de
        // police) ne doit pas être confondu avec un changement de ligne.
        let display = DisplayList {
            items: vec![
                glyph_at('x', 0.0, 700.0, 12.0),
                glyph_at('2', 8.0, 703.0, 8.0), // exposant, légèrement plus haut
            ],
        };
        assert_eq!(extract_text(&display), "x2");
    }

    #[test]
    fn glyphs_without_unicode_are_skipped() {
        let mut skipped = glyph_at('?', 0.0, 700.0, 12.0);
        if let DisplayItem::Glyph { unicode, .. } = &mut skipped {
            *unicode = None;
        }
        let display = DisplayList {
            items: vec![skipped, glyph_at('Y', 8.0, 700.0, 12.0)],
        };
        assert_eq!(extract_text(&display), "Y");
    }

    #[test]
    fn real_fixture_extracts_expected_sentence() {
        use pdf_core::interp::Interpreter;
        use pdf_core::Document;

        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/multipage_classic_xref.pdf").to_vec();
        let doc = Document::open(bytes).unwrap();
        let page = doc.page(0).unwrap();
        let content = doc.page_content(&page).unwrap();
        let display = Interpreter::run_page(&doc, page.resources.clone(), &content).unwrap();

        assert_eq!(extract_text(&display), "Page 1 - Hello, PDF Manager!");
    }

    #[test]
    fn find_matches_locates_a_case_insensitive_substring() {
        let display = DisplayList {
            items: vec![
                glyph_at('H', 0.0, 700.0, 12.0),
                glyph_at('i', 8.0, 700.0, 12.0),
            ],
        };
        let page_text = extract_page_text(&display);
        let matches = page_text.find_matches("HI");
        assert_eq!(matches.len(), 1);
        let rect = matches[0];
        // Le rectangle fusionné doit couvrir les deux glyphes : de x=0 (H)
        // jusqu'à la fin du second (i, à x=8).
        assert_eq!(rect.x0, 0.0);
        assert!(rect.x1 > 8.0);
    }

    #[test]
    fn find_matches_returns_one_rect_per_occurrence() {
        // "ab ab" (avec un espace, lui-même un glyphe) : deux occurrences de
        // "ab" attendues.
        let display = DisplayList {
            items: vec![
                glyph_at('a', 0.0, 700.0, 12.0),
                glyph_at('b', 8.0, 700.0, 12.0),
                glyph_at(' ', 16.0, 700.0, 12.0),
                glyph_at('a', 24.0, 700.0, 12.0),
                glyph_at('b', 32.0, 700.0, 12.0),
            ],
        };
        let page_text = extract_page_text(&display);
        assert_eq!(page_text.find_matches("ab").len(), 2);
        assert_eq!(page_text.find_matches("zz").len(), 0);
        assert_eq!(page_text.find_matches("").len(), 0);
    }

    #[test]
    fn find_matches_ignores_line_break_placeholder_positions() {
        // Un match qui engloberait le saut de ligne inséré (`rects[i] ==
        // None`) doit tout de même produire un rectangle basé sur les
        // caractères qui en ont une, pas paniquer ni retourner un rectangle
        // vide.
        let display = DisplayList {
            items: vec![
                glyph_at('A', 0.0, 700.0, 12.0),
                glyph_at('B', 0.0, 680.0, 12.0), // nouvelle ligne -> insère '\n'
            ],
        };
        let page_text = extract_page_text(&display);
        assert_eq!(page_text.text(), "A\nB");
        let matches = page_text.find_matches("a\nb");
        assert_eq!(matches.len(), 1);
    }
}
