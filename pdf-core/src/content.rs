//! Tokenizer de flux de contenu — architecture.md §4.5.
//!
//! Un flux de contenu de page est une suite d'opérandes suivis d'un
//! opérateur (notation postfixée), p. ex. `1 0 0 1 72 720 cm`. Contrairement
//! aux objets COS, il n'y a jamais de référence indirecte (`N G R`) dans un
//! flux de contenu : on peut donc réutiliser le `Lexer` directement, sans le
//! `Parser` (qui gère la résolution de références, hors sujet ici).

use crate::error::{PdfError, Result};
use crate::lexer::{Lexer, Token};
use crate::object::{Dictionary, Object};

#[derive(Debug, Clone, PartialEq)]
pub struct ContentInstruction {
    pub operator: String,
    pub operands: Vec<Object>,
}

/// Parse un flux de contenu déjà décodé (post-filtres) en une suite
/// d'instructions. Tolérant : une image inline (`BI`...`ID`...`EI`) est
/// repérée et ses données binaires sautées plutôt que décodées (le décodage
/// des images est prévu pour un sprint ultérieur — voir sprint.md).
pub fn parse_content_stream(data: &[u8]) -> Result<Vec<ContentInstruction>> {
    let mut lexer = Lexer::new(data);
    let mut instructions = Vec::new();
    let mut operands: Vec<Object> = Vec::new();

    while let Some(token) = lexer.next_token()? {
        match token {
            Token::Keyword(kw) => match kw.as_str() {
                "true" => operands.push(Object::Boolean(true)),
                "false" => operands.push(Object::Boolean(false)),
                "null" => operands.push(Object::Null),
                "BI" => {
                    skip_inline_image(&mut lexer)?;
                    operands.clear();
                }
                operator => {
                    instructions.push(ContentInstruction {
                        operator: operator.to_string(),
                        operands: std::mem::take(&mut operands),
                    });
                }
            },
            other => operands.push(parse_value_from(&mut lexer, other)?),
        }
    }

    Ok(instructions)
}

fn parse_value_from(lexer: &mut Lexer, token: Token) -> Result<Object> {
    match token {
        Token::Integer(n) => Ok(Object::Integer(n)),
        Token::Real(f) => Ok(Object::Real(f)),
        Token::LiteralString(s) | Token::HexString(s) => Ok(Object::String(s)),
        Token::Name(n) => Ok(Object::Name(n)),
        Token::ArrayStart => parse_array(lexer),
        Token::DictStart => parse_dict(lexer),
        Token::Keyword(kw) if kw == "true" => Ok(Object::Boolean(true)),
        Token::Keyword(kw) if kw == "false" => Ok(Object::Boolean(false)),
        Token::Keyword(kw) if kw == "null" => Ok(Object::Null),
        other => Err(PdfError::InvalidObject(
            lexer.pos(),
            format!("unexpected token as content operand value: {other:?}"),
        )),
    }
}

fn parse_array(lexer: &mut Lexer) -> Result<Object> {
    let mut items = Vec::new();
    loop {
        let Some(token) = lexer.next_token()? else {
            return Err(PdfError::UnexpectedEof(lexer.pos()));
        };
        if token == Token::ArrayEnd {
            break;
        }
        items.push(parse_value_from(lexer, token)?);
    }
    Ok(Object::Array(items))
}

fn parse_dict(lexer: &mut Lexer) -> Result<Object> {
    let mut dict = Dictionary::new();
    loop {
        let Some(token) = lexer.next_token()? else {
            return Err(PdfError::UnexpectedEof(lexer.pos()));
        };
        if token == Token::DictEnd {
            break;
        }
        let Token::Name(key) = token else {
            return Err(PdfError::InvalidObject(
                lexer.pos(),
                "dictionary key must be a Name".to_string(),
            ));
        };
        let Some(value_token) = lexer.next_token()? else {
            return Err(PdfError::UnexpectedEof(lexer.pos()));
        };
        let value = parse_value_from(lexer, value_token)?;
        dict.insert(key, value);
    }
    Ok(Object::Dictionary(dict))
}

/// Consomme le dictionnaire abrégé d'une image inline (jusqu'à `ID`), puis
/// ses données binaires jusqu'à `EI`. Utilise `/L` (longueur explicite,
/// rare en pratique) quand disponible ; sinon recherche heuristique de `EI`
/// entouré d'espaces — limite connue, comme chez la plupart des lecteurs
/// légers (voir sprint.md, Phase 6/7).
fn skip_inline_image(lexer: &mut Lexer) -> Result<()> {
    let mut explicit_length: Option<usize> = None;
    let mut pending_key: Option<String> = None;

    loop {
        match lexer.next_token()? {
            Some(Token::Keyword(kw)) if kw == "ID" => break,
            Some(Token::Name(name)) => {
                pending_key = Some(name);
            }
            Some(Token::Integer(n)) => {
                if let Some(key) = pending_key.take() {
                    if key == "L" || key == "Length" {
                        explicit_length = Some(n.max(0) as usize);
                    }
                }
            }
            Some(_) => {
                pending_key = None;
            }
            None => return Err(PdfError::UnexpectedEof(lexer.pos())),
        }
    }

    // Après `ID`, un unique caractère blanc sépare le dictionnaire des
    // données binaires (ISO 32000-1 §8.9.7).
    if lexer
        .remaining()
        .first()
        .is_some_and(|b| b.is_ascii_whitespace())
    {
        lexer.seek(lexer.pos() + 1);
    }

    if let Some(len) = explicit_length {
        lexer.seek(lexer.pos() + len);
        skip_to_ei_keyword(lexer)?;
        return Ok(());
    }

    // Heuristique : cherche `EI` précédé et suivi d'un caractère non-régulier.
    let remaining = lexer.remaining();
    let mut search_from = 0usize;
    loop {
        let Some(rel) = find_subslice(&remaining[search_from..], b"EI") else {
            return Err(PdfError::UnexpectedEof(lexer.pos()));
        };
        let pos = search_from + rel;
        let preceded_ok = pos == 0 || remaining[pos - 1].is_ascii_whitespace();
        let followed_ok = remaining
            .get(pos + 2)
            .is_none_or(|&b| b.is_ascii_whitespace());
        if preceded_ok && followed_ok {
            lexer.seek(lexer.pos() + pos + 2);
            return Ok(());
        }
        search_from = pos + 2;
    }
}

fn skip_to_ei_keyword(lexer: &mut Lexer) -> Result<()> {
    loop {
        match lexer.next_token()? {
            Some(Token::Keyword(kw)) if kw == "EI" => return Ok(()),
            Some(_) => continue,
            None => return Err(PdfError::UnexpectedEof(lexer.pos())),
        }
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_instructions() {
        let data = b"q 1 0 0 1 72 720 cm 0 0 1 rg 100 200 50 50 re f Q";
        let instrs = parse_content_stream(data).unwrap();
        let ops: Vec<&str> = instrs.iter().map(|i| i.operator.as_str()).collect();
        assert_eq!(ops, vec!["q", "cm", "rg", "re", "f", "Q"]);
        assert_eq!(instrs[1].operands.len(), 6); // cm a b c d e f
        assert_eq!(instrs[3].operands.len(), 4); // re x y w h
    }

    #[test]
    fn parses_text_showing_operators() {
        let data = b"BT /F1 24 Tf 72 720 Td (Hello) Tj ET";
        let instrs = parse_content_stream(data).unwrap();
        let ops: Vec<&str> = instrs.iter().map(|i| i.operator.as_str()).collect();
        assert_eq!(ops, vec!["BT", "Tf", "Td", "Tj", "ET"]);
        assert_eq!(instrs[3].operands[0], Object::String(b"Hello".to_vec()));
    }

    #[test]
    fn parses_tj_array_with_adjustments() {
        let data = b"[(A) -120 (B)] TJ";
        let instrs = parse_content_stream(data).unwrap();
        assert_eq!(instrs.len(), 1);
        assert_eq!(instrs[0].operator, "TJ");
        let Object::Array(items) = &instrs[0].operands[0] else {
            panic!("expected array operand");
        };
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn skips_inline_image_without_length() {
        // Données binaires arbitraires entre ID et EI (y compris un octet
        // qui pourrait ressembler à un délimiteur PDF).
        let mut data = b"BI /W 1 /H 1 /BPC 8 /CS /G ID ".to_vec();
        data.extend_from_slice(&[0xFF, 0x00, 0x7D, 0x28]);
        data.extend_from_slice(b" EI n");
        let instrs = parse_content_stream(&data).unwrap();
        let ops: Vec<&str> = instrs.iter().map(|i| i.operator.as_str()).collect();
        assert_eq!(ops, vec!["n"]);
    }

    #[test]
    fn skips_inline_image_with_explicit_length() {
        let mut data = b"BI /W 1 /H 1 /L 4 ID ".to_vec();
        data.extend_from_slice(&[0xFF, 0x00, 0xFF, 0x00]);
        data.extend_from_slice(b" EI n");
        let instrs = parse_content_stream(&data).unwrap();
        let ops: Vec<&str> = instrs.iter().map(|i| i.operator.as_str()).collect();
        assert_eq!(ops, vec!["n"]);
    }
}
