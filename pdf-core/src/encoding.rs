//! Encodages simples (`/Encoding`) — ISO 32000-1 Annexe D. Couvre
//! `WinAnsiEncoding` et `StandardEncoding` (les deux tables de loin les plus
//! utilisées pour les polices simples non-CJK), plus une résolution de noms
//! de glyphes (Adobe Glyph List) pour `/Differences`.
//!
//! `MacRomanEncoding`, `Symbol`/`ZapfDingbats` (encodages internes non
//! standard) et les polices composites CID (2 octets/code, `/Encoding`
//! nommant un CMap) restent à faire — voir sprint.md, Sprint 7-8+.

/// `StandardEncoding` : table historique PostScript, base par défaut quand
/// aucun `/Encoding` n'est spécifié pour une police non-symbolique.
pub const STANDARD_ENCODING: [Option<char>; 256] = build_standard_encoding();

/// `WinAnsiEncoding` : proche de Windows-1252, encodage par défaut le plus
/// courant en pratique (polices non intégrées générées par la plupart des
/// outils, dont reportlab).
pub const WIN_ANSI_ENCODING: [Option<char>; 256] = build_win_ansi_encoding();

const fn ascii_table() -> [Option<char>; 256] {
    let mut table = [None; 256];
    let mut i = 0x20u8;
    while i < 0x7F {
        table[i as usize] = Some(i as char);
        i += 1;
    }
    table
}

const fn build_standard_encoding() -> [Option<char>; 256] {
    let mut table = ascii_table();
    // StandardEncoding diverge de l'ASCII sur quelques codes bas et sur le
    // haut de la table (accents, ligatures) ; on couvre les cas les plus
    // fréquents rencontrés en pratique occidentale.
    table[0x27] = Some('\u{2019}'); // quoteright
    table[0x60] = Some('\u{2018}'); // quoteleft
    table
}

const fn build_win_ansi_encoding() -> [Option<char>; 256] {
    let mut table = ascii_table();
    // Plage haute (0x80-0xFF) : correspond à Windows-1252 pour la quasi
    // totalité des codes (quelques positions réservées diffèrent en PDF ;
    // non couvertes ici, cas rares en pratique).
    let high: [(u8, char); 97] = [
        (0x80, '\u{20AC}'),
        (0x82, '\u{201A}'),
        (0x83, '\u{0192}'),
        (0x84, '\u{201E}'),
        (0x85, '\u{2026}'),
        (0x86, '\u{2020}'),
        (0x87, '\u{2021}'),
        (0x88, '\u{02C6}'),
        (0x89, '\u{2030}'),
        (0x8A, '\u{0160}'),
        (0x8B, '\u{2039}'),
        (0x8C, '\u{0152}'),
        (0x8E, '\u{017D}'),
        (0x91, '\u{2018}'),
        (0x92, '\u{2019}'),
        (0x93, '\u{201C}'),
        (0x94, '\u{201D}'),
        (0x95, '\u{2022}'),
        (0x96, '\u{2013}'),
        (0x97, '\u{2014}'),
        (0x98, '\u{02DC}'),
        (0x99, '\u{2122}'),
        (0x9A, '\u{0161}'),
        (0x9B, '\u{203A}'),
        (0x9C, '\u{0153}'),
        (0x9E, '\u{017E}'),
        (0x9F, '\u{0178}'),
        (0xA0, '\u{00A0}'),
        (0xA1, '\u{00A1}'),
        (0xA2, '\u{00A2}'),
        (0xA3, '\u{00A3}'),
        (0xA4, '\u{00A4}'),
        (0xA5, '\u{00A5}'),
        (0xA6, '\u{00A6}'),
        (0xA7, '\u{00A7}'),
        (0xA8, '\u{00A8}'),
        (0xA9, '\u{00A9}'),
        (0xAA, '\u{00AA}'),
        (0xAB, '\u{00AB}'),
        (0xAC, '\u{00AC}'),
        (0xAD, '\u{00AD}'),
        (0xAE, '\u{00AE}'),
        (0xAF, '\u{00AF}'),
        (0xB0, '\u{00B0}'),
        (0xB1, '\u{00B1}'),
        (0xB2, '\u{00B2}'),
        (0xB3, '\u{00B3}'),
        (0xB4, '\u{00B4}'),
        (0xB5, '\u{00B5}'),
        (0xB6, '\u{00B6}'),
        (0xB7, '\u{00B7}'),
        (0xB8, '\u{00B8}'),
        (0xB9, '\u{00B9}'),
        (0xBA, '\u{00BA}'),
        (0xBB, '\u{00BB}'),
        (0xBC, '\u{00BC}'),
        (0xBD, '\u{00BD}'),
        (0xBE, '\u{00BE}'),
        (0xBF, '\u{00BF}'),
        (0xC0, '\u{00C0}'),
        (0xC1, '\u{00C1}'),
        (0xC2, '\u{00C2}'),
        (0xC3, '\u{00C3}'),
        (0xC4, '\u{00C4}'),
        (0xC5, '\u{00C5}'),
        (0xC6, '\u{00C6}'),
        (0xC7, '\u{00C7}'),
        (0xC8, '\u{00C8}'),
        (0xC9, '\u{00C9}'),
        (0xCA, '\u{00CA}'),
        (0xCB, '\u{00CB}'),
        (0xCC, '\u{00CC}'),
        (0xCD, '\u{00CD}'),
        (0xCE, '\u{00CE}'),
        (0xCF, '\u{00CF}'),
        (0xD0, '\u{00D0}'),
        (0xD1, '\u{00D1}'),
        (0xD2, '\u{00D2}'),
        (0xD3, '\u{00D3}'),
        (0xD4, '\u{00D4}'),
        (0xD5, '\u{00D5}'),
        (0xD6, '\u{00D6}'),
        (0xD7, '\u{00D7}'),
        (0xD8, '\u{00D8}'),
        (0xD9, '\u{00D9}'),
        (0xDA, '\u{00DA}'),
        (0xDB, '\u{00DB}'),
        (0xDC, '\u{00DC}'),
        (0xDD, '\u{00DD}'),
        (0xDE, '\u{00DE}'),
        (0xDF, '\u{00DF}'),
        (0xE0, '\u{00E0}'),
        (0xE1, '\u{00E1}'),
        (0xE2, '\u{00E2}'),
        (0xE3, '\u{00E3}'),
        (0xE4, '\u{00E4}'),
        (0xE5, '\u{00E5}'),
    ];
    let mut i = 0;
    while i < high.len() {
        let (code, ch) = high[i];
        table[code as usize] = Some(ch);
        i += 1;
    }
    // 0xE6-0xFF suivent Latin-1 de la même manière ; complétés par une
    // boucle directe (offset constant) plutôt que de tout lister.
    let mut code = 0xE6u16;
    while code <= 0xFF {
        table[code as usize] = char::from_u32(code as u32);
        code += 1;
    }
    table
}

/// Résout un nom de glyphe (`/Differences`) en caractère Unicode. Couvre les
/// noms ASCII standard (`space`, `A`, `zero`...) et un sous-ensemble courant
/// de l'Adobe Glyph List pour les caractères latins accentués. Un nom
/// inconnu retourne `None` plutôt qu'un caractère de remplacement.
pub fn glyph_name_to_unicode(name: &str) -> Option<char> {
    // `uniXXXX` / `uXXXX` : forme normalisée de l'AGL pour un code point direct.
    if let Some(hex) = name.strip_prefix("uni") {
        if let Ok(cp) = u32::from_str_radix(hex, 16) {
            return char::from_u32(cp);
        }
    }
    if let Some(hex) = name.strip_prefix('u') {
        if hex.len() >= 4 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            if let Ok(cp) = u32::from_str_radix(hex, 16) {
                return char::from_u32(cp);
            }
        }
    }

    Some(match name {
        "space" => ' ',
        "exclam" => '!',
        "quotedbl" => '"',
        "numbersign" => '#',
        "dollar" => '$',
        "percent" => '%',
        "ampersand" => '&',
        "quotesingle" | "quoteright" => '\'',
        "parenleft" => '(',
        "parenright" => ')',
        "asterisk" => '*',
        "plus" => '+',
        "comma" => ',',
        "hyphen" | "minus" => '-',
        "period" => '.',
        "slash" => '/',
        "zero" => '0',
        "one" => '1',
        "two" => '2',
        "three" => '3',
        "four" => '4',
        "five" => '5',
        "six" => '6',
        "seven" => '7',
        "eight" => '8',
        "nine" => '9',
        "colon" => ':',
        "semicolon" => ';',
        "less" => '<',
        "equal" => '=',
        "greater" => '>',
        "question" => '?',
        "at" => '@',
        "bracketleft" => '[',
        "backslash" => '\\',
        "bracketright" => ']',
        "asciicircum" => '^',
        "underscore" => '_',
        "grave" | "quoteleft" => '`',
        "braceleft" => '{',
        "bar" => '|',
        "braceright" => '}',
        "asciitilde" => '~',
        "eacute" => '\u{00E9}',
        "egrave" => '\u{00E8}',
        "ecirc" => '\u{00EA}',
        "edieresis" => '\u{00EB}',
        "agrave" => '\u{00E0}',
        "acircumflex" => '\u{00E2}',
        "adieresis" => '\u{00E4}',
        "ccedilla" => '\u{00E7}',
        "ntilde" => '\u{00F1}',
        "ograve" => '\u{00F2}',
        "ocircumflex" => '\u{00F4}',
        "odieresis" => '\u{00F6}',
        "ugrave" => '\u{00F9}',
        "ucircumflex" => '\u{00FB}',
        "udieresis" => '\u{00FC}',
        "Eacute" => '\u{00C9}',
        "Egrave" => '\u{00C8}',
        "Agrave" => '\u{00C0}',
        "Ccedilla" => '\u{00C7}',
        "Ntilde" => '\u{00D1}',
        _ if name.len() == 1 => name.chars().next().unwrap(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_range_is_identity() {
        assert_eq!(WIN_ANSI_ENCODING[b'A' as usize], Some('A'));
        assert_eq!(STANDARD_ENCODING[b'z' as usize], Some('z'));
    }

    #[test]
    fn win_ansi_high_range_maps_to_latin1() {
        assert_eq!(WIN_ANSI_ENCODING[0xE9], Some('\u{00E9}')); // eacute
        assert_eq!(WIN_ANSI_ENCODING[0x80], Some('\u{20AC}')); // euro sign
    }

    #[test]
    fn glyph_names_resolve() {
        assert_eq!(glyph_name_to_unicode("eacute"), Some('\u{00E9}'));
        assert_eq!(glyph_name_to_unicode("space"), Some(' '));
        assert_eq!(glyph_name_to_unicode("uni00E9"), Some('\u{00E9}'));
        assert_eq!(glyph_name_to_unicode("nonexistentglyph"), None);
    }
}
