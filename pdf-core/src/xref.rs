//! Table de références croisées classique + reconstruction de secours —
//! architecture.md §4.2. Les cross-reference streams / object streams
//! (PDF 1.5+) sont prévus pour une itération ultérieure (sprint.md, Sprint 3-4
//! partie avancée).

use crate::error::{PdfError, Result};
use crate::lexer::{Lexer, Token};
use crate::object::Dictionary;
use crate::parser::Parser;
use std::collections::BTreeMap;

/// Table num d'objet -> offset dans le fichier.
#[derive(Debug, Clone, Default)]
pub struct XrefTable {
    pub offsets: BTreeMap<u32, usize>,
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

/// Parse la table xref classique (+ trailer) à l'offset donné, en suivant
/// les chaînes `/Prev` pour les mises à jour incrémentales.
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
                for (num, off) in section_table.offsets {
                    table.offsets.entry(num).or_insert(off);
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

    if table.offsets.is_empty() {
        return reconstruct_by_scan(data);
    }

    Ok((table, trailer))
}

fn parse_xref_section(data: &[u8], offset: usize) -> Result<(XrefTable, Dictionary)> {
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
                        table.offsets.insert((start + i) as u32, off as usize);
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

/// Reconstruction de secours : scanne tout le fichier à la recherche des
/// motifs `N G obj` pour reconstituer une table d'offsets, puis localise le
/// trailer le plus plausible (dernier `trailer` du fichier, ou dictionnaire
/// `/Type /Catalog` en dernier recours).
fn reconstruct_by_scan(data: &[u8]) -> Result<(XrefTable, Dictionary)> {
    let mut table = XrefTable::default();
    let mut lexer = Lexer::new(data);
    let mut pending: Vec<(usize, Token)> = Vec::new();

    loop {
        let pos = lexer.pos();
        let Some(token) = lexer.next_token()? else {
            break;
        };
        pending.push((pos, token.clone()));
        if pending.len() > 3 {
            pending.remove(0);
        }
        if let Token::Keyword(kw) = &token {
            if kw == "obj" && pending.len() == 3 {
                if let (
                    (start_pos, Token::Integer(num)),
                    (_, Token::Integer(_gen)),
                    (_, Token::Keyword(_)),
                ) = (&pending[0], &pending[1], &pending[2])
                {
                    if *num >= 0 {
                        table.offsets.insert(*num as u32, *start_pos);
                    }
                }
            }
        }
    }

    if table.offsets.is_empty() {
        return Err(PdfError::InvalidXref(
            "no indirect objects found while reconstructing xref".into(),
        ));
    }

    // Cherche le dernier trailer explicite dans le fichier.
    if let Some(trailer_pos) = find_last_trailer(data) {
        let mut parser = Parser::with_pos(data, trailer_pos);
        if let Ok(obj) = parser.parse_object() {
            if let Some(dict) = obj.as_dict() {
                return Ok((table, dict.clone()));
            }
        }
    }

    Ok((table, Dictionary::new()))
}

fn find_last_trailer(data: &[u8]) -> Option<usize> {
    const NEEDLE: &[u8] = b"trailer";
    let rel = data.windows(NEEDLE.len()).rposition(|w| w == NEEDLE)?;
    Some(rel + NEEDLE.len())
}
