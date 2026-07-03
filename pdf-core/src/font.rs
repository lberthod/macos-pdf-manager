//! Métriques et encodage des polices simples (Type1/TrueType 1 octet) —
//! architecture.md §4.6. Fournit à l'interpréteur de contenu (`interp.rs`)
//! de quoi remplacer l'avance de texte placeholder par une largeur réelle,
//! et le code de caractère brut par un caractère Unicode.
//!
//! Limitations connues (voir sprint.md, Sprint 7-8+) :
//! - Polices composites (`/Type0`, CID, codes 2 octets) : non gérées, un
//!   appelant doit détecter `Font::is_composite()` et garder son
//!   comportement de repli.
//! - `/ToUnicode` (CMap dédié, prioritaire sur l'encodage de base quand
//!   présent) : non lu, seul `/Encoding` (+ `/Differences`) est utilisé.
//! - Largeurs des polices standard non intégrées : seule Helvetica dispose
//!   d'une table AFM complète (`HELVETICA_WIDTHS`) ; les 13 autres polices
//!   standard retombent sur `DEFAULT_WIDTH`.
//! - `MacRomanEncoding` est approximé par `WinAnsiEncoding` (tables réelles
//!   distinctes au-delà de l'ASCII) faute de table dédiée pour l'instant.

use crate::document::Document;
use crate::encoding::{glyph_name_to_unicode, STANDARD_ENCODING, WIN_ANSI_ENCODING};
use crate::error::Result;
use crate::object::{Dictionary, Object};

/// Avance de repli ultime (en millièmes d'em) quand ni `/Widths` ni la table
/// Helvetica ne couvrent un code donné.
const DEFAULT_WIDTH: f64 = 500.0;

pub struct Font {
    subtype: String,
    encoding: [Option<char>; 256],
    widths: [Option<f64>; 256],
    use_helvetica_fallback: bool,
}

impl Font {
    /// Construit une police à partir de son dictionnaire `/Font` déjà résolu.
    pub fn load(doc: &Document, dict: &Dictionary) -> Result<Font> {
        let subtype = dict
            .get("Subtype")
            .and_then(|o| o.as_name())
            .unwrap_or("")
            .to_string();

        let mut encoding = WIN_ANSI_ENCODING;
        let mut differences: Vec<(i64, String)> = Vec::new();

        if let Some(enc_obj) = dict.get("Encoding") {
            let enc = doc.get(enc_obj)?;
            match &enc {
                Object::Name(name) => {
                    encoding = base_encoding_by_name(name).unwrap_or(STANDARD_ENCODING);
                }
                Object::Dictionary(enc_dict) => {
                    if let Some(base_name) = enc_dict.get("BaseEncoding").and_then(|o| o.as_name())
                    {
                        encoding = base_encoding_by_name(base_name).unwrap_or(WIN_ANSI_ENCODING);
                    }
                    if let Some(Object::Array(items)) = enc_dict.get("Differences") {
                        let mut current_code = 0i64;
                        for item in items {
                            match item {
                                Object::Integer(n) => current_code = *n,
                                Object::Name(name) => {
                                    differences.push((current_code, name.clone()));
                                    current_code += 1;
                                }
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        for (code, name) in differences {
            if (0..256).contains(&code) {
                if let Some(ch) = glyph_name_to_unicode(&name) {
                    encoding[code as usize] = Some(ch);
                }
            }
        }

        let mut widths: [Option<f64>; 256] = [None; 256];
        let first_char = dict.get("FirstChar").and_then(|o| o.as_int());
        let widths_obj = dict.get("Widths").map(|o| doc.get(o)).transpose()?;
        if let (Some(first_char), Some(items)) =
            (first_char, widths_obj.as_ref().and_then(|o| o.as_array()))
        {
            for (i, w) in items.iter().enumerate() {
                let code = first_char + i as i64;
                if (0..256).contains(&code) {
                    if let Some(width) = number(w) {
                        widths[code as usize] = Some(width);
                    }
                }
            }
        }

        let use_helvetica_fallback = subtype != "Type0" && widths.iter().all(|w| w.is_none());

        Ok(Font {
            subtype,
            encoding,
            widths,
            use_helvetica_fallback,
        })
    }

    /// Décode un code de caractère 1 octet (polices simples uniquement) en
    /// `(unicode résolu, largeur en 1/1000 em)`.
    pub fn decode_simple(&self, code: u8) -> (Option<char>, f64) {
        let unicode = self.encoding[code as usize];
        let width = self.widths[code as usize]
            .or_else(|| {
                if self.use_helvetica_fallback {
                    helvetica_width(code)
                } else {
                    None
                }
            })
            .unwrap_or(DEFAULT_WIDTH);
        (unicode, width)
    }

    pub fn is_composite(&self) -> bool {
        self.subtype == "Type0"
    }
}

fn number(obj: &Object) -> Option<f64> {
    match obj {
        Object::Integer(n) => Some(*n as f64),
        Object::Real(f) => Some(*f),
        _ => None,
    }
}

fn base_encoding_by_name(name: &str) -> Option<[Option<char>; 256]> {
    match name {
        "WinAnsiEncoding" => Some(WIN_ANSI_ENCODING),
        "StandardEncoding" => Some(STANDARD_ENCODING),
        "MacRomanEncoding" => Some(WIN_ANSI_ENCODING), // approximation, voir limitations ci-dessus.
        _ => None,
    }
}

/// Largeurs Helvetica (Adobe Core 14 AFM), millièmes d'em, codes ASCII
/// imprimables 32-126. Utilisées en repli quand une police standard non
/// intégrée ne fournit pas de `/Widths` — cas normal pour les 14 polices
/// standard, que le lecteur est censé connaître nativement.
fn helvetica_width(code: u8) -> Option<f64> {
    Some(match code {
        32 => 278.0,
        33 => 278.0,
        34 => 355.0,
        35 => 556.0,
        36 => 556.0,
        37 => 889.0,
        38 => 667.0,
        39 => 191.0,
        40 => 333.0,
        41 => 333.0,
        42 => 389.0,
        43 => 584.0,
        44 => 278.0,
        45 => 333.0,
        46 => 278.0,
        47 => 278.0,
        48..=57 => 556.0,
        58 => 278.0,
        59 => 278.0,
        60 => 584.0,
        61 => 584.0,
        62 => 584.0,
        63 => 556.0,
        64 => 1015.0,
        65 => 667.0,
        66 => 667.0,
        67 => 722.0,
        68 => 722.0,
        69 => 667.0,
        70 => 611.0,
        71 => 778.0,
        72 => 722.0,
        73 => 278.0,
        74 => 500.0,
        75 => 667.0,
        76 => 556.0,
        77 => 833.0,
        78 => 722.0,
        79 => 778.0,
        80 => 667.0,
        81 => 778.0,
        82 => 722.0,
        83 => 667.0,
        84 => 611.0,
        85 => 722.0,
        86 => 667.0,
        87 => 944.0,
        88 => 667.0,
        89 => 667.0,
        90 => 611.0,
        91 => 278.0,
        92 => 278.0,
        93 => 278.0,
        94 => 469.0,
        95 => 556.0,
        96 => 333.0,
        97 => 556.0,
        98 => 556.0,
        99 => 500.0,
        100 => 556.0,
        101 => 556.0,
        102 => 278.0,
        103 => 556.0,
        104 => 556.0,
        105 => 222.0,
        106 => 222.0,
        107 => 500.0,
        108 => 222.0,
        109 => 833.0,
        110 => 556.0,
        111 => 556.0,
        112 => 556.0,
        113 => 556.0,
        114 => 333.0,
        115 => 500.0,
        116 => 278.0,
        117 => 556.0,
        118 => 500.0,
        119 => 722.0,
        120 => 500.0,
        121 => 500.0,
        122 => 500.0,
        123 => 334.0,
        124 => 260.0,
        125 => 334.0,
        126 => 584.0,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font_dict(entries: &[(&str, Object)]) -> Dictionary {
        let mut dict = Dictionary::new();
        for (k, v) in entries {
            dict.insert(*k, v.clone());
        }
        dict
    }

    fn dummy_doc() -> Document {
        // Document minimal juste pour permettre `doc.get()` sur des objets
        // directs (aucune référence indirecte utilisée dans ces tests).
        let body = concat!(
            "%PDF-1.7\n",
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
            "2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n",
        );
        let mut bytes = body.as_bytes().to_vec();
        let off1 = bytes.windows(7).position(|w| w == b"1 0 obj").unwrap();
        let off2 = bytes.windows(7).position(|w| w == b"2 0 obj").unwrap();
        let xref_offset = bytes.len();
        let xref = format!(
            "xref\n0 3\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \ntrailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF"
        );
        bytes.extend_from_slice(xref.as_bytes());
        Document::open(bytes).unwrap()
    }

    #[test]
    fn helvetica_fallback_widths_without_widths_array() {
        let doc = dummy_doc();
        let dict = font_dict(&[
            ("Subtype", Object::Name("Type1".into())),
            ("BaseFont", Object::Name("Helvetica".into())),
            ("Encoding", Object::Name("WinAnsiEncoding".into())),
        ]);
        let font = Font::load(&doc, &dict).unwrap();
        let (unicode, width) = font.decode_simple(b'A');
        assert_eq!(unicode, Some('A'));
        assert_eq!(width, 667.0);
    }

    #[test]
    fn explicit_widths_array_take_priority() {
        let doc = dummy_doc();
        let dict = font_dict(&[
            ("Subtype", Object::Name("TrueType".into())),
            ("FirstChar", Object::Integer(65)),
            (
                "Widths",
                Object::Array(vec![Object::Integer(999), Object::Integer(999)]),
            ),
        ]);
        let font = Font::load(&doc, &dict).unwrap();
        let (_, width) = font.decode_simple(b'A');
        assert_eq!(width, 999.0);
    }

    #[test]
    fn differences_override_base_encoding() {
        let doc = dummy_doc();
        let mut enc_dict = Dictionary::new();
        enc_dict.insert("BaseEncoding", Object::Name("WinAnsiEncoding".into()));
        enc_dict.insert(
            "Differences",
            Object::Array(vec![Object::Integer(65), Object::Name("eacute".into())]),
        );
        let dict = font_dict(&[
            ("Subtype", Object::Name("Type1".into())),
            ("Encoding", Object::Dictionary(enc_dict)),
        ]);
        let font = Font::load(&doc, &dict).unwrap();
        let (unicode, _) = font.decode_simple(65);
        assert_eq!(unicode, Some('\u{00E9}'));
    }

    #[test]
    fn type0_is_reported_composite() {
        let doc = dummy_doc();
        let dict = font_dict(&[("Subtype", Object::Name("Type0".into()))]);
        let font = Font::load(&doc, &dict).unwrap();
        assert!(font.is_composite());
    }
}
