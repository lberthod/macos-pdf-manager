//! Table de références croisées : format classique et cross-reference
//! streams (PDF 1.5+), plus reconstruction de secours — architecture.md §4.2.

use crate::error::{PdfError, Result};
use crate::filters::decode_stream;
use crate::lexer::{Lexer, Token};
use crate::object::{Dictionary, Object};
use crate::parser::Parser;
use std::collections::BTreeMap;

/// Emplacement d'un objet indirect : soit un offset direct dans le fichier,
/// soit un objet compressé dans un object stream (`/Type /ObjStm`, PDF 1.5+).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XrefEntry {
    Offset(usize),
    Compressed { stream_num: u32, index: u32 },
}

/// Table num d'objet -> emplacement.
#[derive(Debug, Clone, Default)]
pub struct XrefTable {
    pub entries: BTreeMap<u32, XrefEntry>,
}

/// Cherche `startxref` en partant de la fin du fichier et lit l'offset qui suit.
fn find_startxref_offset(data: &[u8]) -> Result<usize> {
    const NEEDLE: &[u8] = b"startxref";
    let tail_start = data.len().saturating_sub(2048);
    let search_zone = &data[tail_start..];
    let rel = search_zone
        .windows(NEEDLE.len())
        .rposition(|w| w == NEEDLE)
        .ok_or_else(|| PdfError::InvalidXref("`startxref` keyword not found".into()))?;
    let abs = tail_start + rel + NEEDLE.len();
    let mut lexer = Lexer::with_pos(data, abs);
    match lexer.next_token()? {
        Some(Token::Integer(n)) if n >= 0 => Ok(n as usize),
        other => Err(PdfError::InvalidXref(format!(
            "expected offset after `startxref`, found {other:?}"
        ))),
    }
}

/// Parse la table xref (classique ou stream) à l'offset donné, en suivant
/// les chaînes `/Prev` (et `/XRefStm` pour les mises à jour hybrides) pour
/// les mises à jour incrémentales.
pub fn parse_xref_chain(data: &[u8]) -> Result<(XrefTable, Dictionary)> {
    let Ok(start_offset) = find_startxref_offset(data) else {
        return reconstruct_by_scan(data);
    };
    let mut table = XrefTable::default();
    let mut trailer = Dictionary::new();
    let mut next_offset = Some(start_offset);
    let mut visited = Vec::new();

    while let Some(offset) = next_offset {
        if visited.contains(&offset) {
            break; // évite les boucles /Prev malformées
        }
        visited.push(offset);

        match parse_xref_section(data, offset) {
            Ok((section_table, section_trailer)) => {
                // Les entrées les plus récentes (première section lue) priment.
                for (num, entry) in section_table.entries {
                    table.entries.entry(num).or_insert(entry);
                }
                for (key, value) in section_trailer.iter() {
                    trailer
                        .0
                        .entry(key.clone())
                        .or_insert_with(|| value.clone());
                }
                next_offset = section_trailer
                    .get("Prev")
                    .and_then(|o| o.as_int())
                    .map(|n| n as usize);
            }
            Err(_) => break,
        }
    }

    if table.entries.is_empty() {
        return reconstruct_by_scan(data);
    }

    Ok((table, trailer))
}

/// Distingue une section xref classique (`xref` ... `trailer`) d'un
/// cross-reference stream (objet indirect dont le dict a `/Type /XRef`).
fn parse_xref_section(data: &[u8], offset: usize) -> Result<(XrefTable, Dictionary)> {
    let mut probe = Lexer::with_pos(data, offset);
    match probe.next_token()? {
        Some(Token::Keyword(kw)) if kw == "xref" => parse_classic_xref_section(data, offset),
        _ => parse_xref_stream_section(data, offset),
    }
}

fn parse_classic_xref_section(data: &[u8], offset: usize) -> Result<(XrefTable, Dictionary)> {
    let mut lexer = Lexer::with_pos(data, offset);
    match lexer.next_token()? {
        Some(Token::Keyword(kw)) if kw == "xref" => {}
        other => {
            return Err(PdfError::InvalidXref(format!(
                "expected `xref` keyword at offset {offset}, found {other:?}"
            )))
        }
    }

    let mut table = XrefTable::default();
    loop {
        let checkpoint = lexer.pos();
        match lexer.next_token()? {
            Some(Token::Integer(start)) => {
                let Some(Token::Integer(count)) = lexer.next_token()? else {
                    return Err(PdfError::InvalidXref(
                        "malformed xref subsection header".into(),
                    ));
                };
                for i in 0..count {
                    let Some(Token::Integer(off)) = lexer.next_token()? else {
                        return Err(PdfError::InvalidXref("malformed xref entry".into()));
                    };
                    let Some(Token::Integer(_gen)) = lexer.next_token()? else {
                        return Err(PdfError::InvalidXref("malformed xref entry".into()));
                    };
                    let Some(Token::Keyword(kind)) = lexer.next_token()? else {
                        return Err(PdfError::InvalidXref("malformed xref entry".into()));
                    };
                    if kind == "n" {
                        table
                            .entries
                            .insert((start + i) as u32, XrefEntry::Offset(off as usize));
                    }
                }
            }
            _ => {
                lexer.seek(checkpoint);
                break;
            }
        }
    }

    match lexer.next_token()? {
        Some(Token::Keyword(kw)) if kw == "trailer" => {}
        other => {
            return Err(PdfError::InvalidXref(format!(
                "expected `trailer` keyword, found {other:?}"
            )))
        }
    }

    let mut parser = Parser::with_pos(data, lexer.pos());
    let trailer_obj = parser.parse_object()?;
    let trailer = trailer_obj
        .as_dict()
        .cloned()
        .ok_or_else(|| PdfError::InvalidXref("trailer is not a dictionary".into()))?;

    Ok((table, trailer))
}

/// Parse un cross-reference stream (PDF 1.5+, ISO 32000-1 §7.5.8) : un objet
/// indirect `/Type /XRef` dont le flux (souvent `FlateDecode` + prédicteur
/// PNG) contient des enregistrements de largeur fixe décrits par `/W`.
fn parse_xref_stream_section(data: &[u8], offset: usize) -> Result<(XrefTable, Dictionary)> {
    let mut parser = Parser::with_pos(data, offset);
    let (_num, _gen, object) = parser.parse_indirect_object()?;
    let Object::Stream(stream) = object else {
        return Err(PdfError::InvalidXref(
            "expected a stream object for cross-reference stream".into(),
        ));
    };

    let dict = stream.dict.clone();
    match dict.get("Type").and_then(|o| o.as_name()) {
        Some("XRef") => {}
        other => {
            return Err(PdfError::InvalidXref(format!(
                "expected /Type /XRef, found {other:?}"
            )))
        }
    }

    let widths: Vec<usize> = dict
        .get("W")
        .and_then(|o| o.as_array())
        .ok_or_else(|| PdfError::MissingKey("W".into()))?
        .iter()
        .map(|o| o.as_int().unwrap_or(0) as usize)
        .collect();
    if widths.len() != 3 {
        return Err(PdfError::InvalidXref(
            "/W must have exactly 3 entries".into(),
        ));
    }
    let (w0, w1, w2) = (widths[0], widths[1], widths[2]);
    let record_len = w0 + w1 + w2;

    let size = dict.get_int("Size").unwrap_or(0);
    let index: Vec<i64> = match dict.get("Index").and_then(|o| o.as_array()) {
        Some(items) => items.iter().filter_map(|o| o.as_int()).collect(),
        None => vec![0, size],
    };

    let decoded = decode_stream(&stream)?;
    if record_len == 0 {
        return Err(PdfError::InvalidXref("/W entries sum to zero".into()));
    }

    let mut table = XrefTable::default();
    let mut cursor = 0usize;
    for pair in index.chunks(2) {
        let [start, count] = [pair[0], *pair.get(1).unwrap_or(&0)];
        for i in 0..count {
            if cursor + record_len > decoded.len() {
                break;
            }
            let record = &decoded[cursor..cursor + record_len];
            cursor += record_len;

            let field_type = if w0 == 0 {
                1 // Par défaut (absence de /W[0]) : entrée "en usage" classique.
            } else {
                be_bytes_to_u64(&record[0..w0])
            };
            let field2 = be_bytes_to_u64(&record[w0..w0 + w1]);
            let field3 = be_bytes_to_u64(&record[w0 + w1..w0 + w1 + w2]);

            let obj_num = (start + i) as u32;
            match field_type {
                0 => {} // objet libre : ignoré.
                1 => {
                    table
                        .entries
                        .insert(obj_num, XrefEntry::Offset(field2 as usize));
                }
                2 => {
                    table.entries.insert(
                        obj_num,
                        XrefEntry::Compressed {
                            stream_num: field2 as u32,
                            index: field3 as u32,
                        },
                    );
                }
                _ => {} // types réservés futurs : ignorés.
            }
        }
    }

    Ok((table, dict))
}

fn be_bytes_to_u64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0u64, |acc, &b| (acc << 8) | b as u64)
}

/// Reconstruction de secours : scanne le fichier **au niveau des octets**
/// (pas via le lexer) à la recherche du motif littéral `obj` précédé de
/// `N G `, pour reconstituer une table d'offsets. Un scan basé sur le lexer
/// échouerait dès qu'un flux compressé contient un octet ressemblant à un
/// délimiteur PDF ; les données brutes de `stream`/`endstream` sont
/// arbitraires et ne suivent pas la syntaxe des objets.
fn reconstruct_by_scan(data: &[u8]) -> Result<(XrefTable, Dictionary)> {
    let mut table = XrefTable::default();
    let mut search_from = 0usize;

    while let Some(rel) = find_subslice(&data[search_from..], b"obj") {
        let obj_pos = search_from + rel;
        search_from = obj_pos + 3;

        // Exclut `endobj` (qui contient `obj` comme sous-chaîne) et exige
        // que `obj` soit suivi d'un caractère non-régulier (espace/délimiteur).
        if obj_pos >= 3 && &data[obj_pos - 3..obj_pos] == b"end" {
            continue;
        }
        if data.get(obj_pos + 3).is_some_and(|&b| is_regular_byte(b)) {
            continue;
        }

        if let Some((num, start_pos)) = parse_header_backwards(data, obj_pos) {
            table.entries.insert(num, XrefEntry::Offset(start_pos));
        }
    }

    if table.entries.is_empty() {
        return Err(PdfError::InvalidXref(
            "no indirect objects found while reconstructing xref".into(),
        ));
    }

    // Cherche le dernier trailer explicite dans le fichier.
    if let Some(trailer_pos) = find_last_trailer(data) {
        let mut parser = Parser::with_pos(data, trailer_pos);
        if let Ok(obj) = parser.parse_object() {
            if let Some(dict) = obj.as_dict() {
                if dict.get("Root").is_some() {
                    return Ok((table, dict.clone()));
                }
            }
        }
    }

    // Dernier recours (pas de trailer exploitable) : cherche un objet
    // `/Type /Catalog` parmi les objets retrouvés et synthétise un trailer.
    if let Some(root_num) = find_catalog_object(data, &table) {
        let mut trailer = Dictionary::new();
        trailer.insert(
            "Root",
            Object::Reference(crate::object::ObjRef::new(root_num, 0)),
        );
        return Ok((table, trailer));
    }

    Ok((table, Dictionary::new()))
}

fn find_catalog_object(data: &[u8], table: &XrefTable) -> Option<u32> {
    for (&num, entry) in &table.entries {
        let XrefEntry::Offset(offset) = entry else {
            continue;
        };
        let mut parser = Parser::with_pos(data, *offset);
        let Ok((_, _, object)) = parser.parse_indirect_object() else {
            continue;
        };
        if let Some(dict) = object.as_dict() {
            if dict.get("Type").and_then(|o| o.as_name()) == Some("Catalog") {
                return Some(num);
            }
        }
    }
    None
}

fn find_last_trailer(data: &[u8]) -> Option<usize> {
    const NEEDLE: &[u8] = b"trailer";
    let rel = data.windows(NEEDLE.len()).rposition(|w| w == NEEDLE)?;
    Some(rel + NEEDLE.len())
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn is_regular_byte(b: u8) -> bool {
    !matches!(
        b,
        b'\0'
            | b'\t'
            | b'\n'
            | 0x0C
            | b'\r'
            | b' '
            | b'('
            | b')'
            | b'<'
            | b'>'
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'/'
            | b'%'
    )
}

/// Depuis la position du mot-clé `obj`, remonte dans le fichier pour lire
/// `N G ` (numéro d'objet, génération) au format brut, sans passer par le
/// lexer. Retourne `(numéro d'objet, offset de début de "N")`.
fn parse_header_backwards(data: &[u8], obj_pos: usize) -> Option<(u32, usize)> {
    let mut i = obj_pos;

    let skip_whitespace_backwards = |data: &[u8], mut i: usize| -> usize {
        while i > 0 && data[i - 1].is_ascii_whitespace() {
            i -= 1;
        }
        i
    };
    let read_digits_backwards = |data: &[u8], mut i: usize| -> (usize, usize) {
        let end = i;
        while i > 0 && data[i - 1].is_ascii_digit() {
            i -= 1;
        }
        (i, end)
    };

    i = skip_whitespace_backwards(data, i);
    let (gen_start, gen_end) = read_digits_backwards(data, i);
    if gen_start == gen_end {
        return None; // pas de génération numérique trouvée.
    }

    i = skip_whitespace_backwards(data, gen_start);
    let (num_start, num_end) = read_digits_backwards(data, i);
    if num_start == num_end {
        return None;
    }

    let num_str = std::str::from_utf8(&data[num_start..num_end]).ok()?;
    let num: u32 = num_str.parse().ok()?;
    Some((num, num_start))
}
