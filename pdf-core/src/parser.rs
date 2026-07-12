//! Parser d'objets COS au-dessus du lexer — architecture.md §4.2.

use crate::error::{PdfError, Result};
use crate::lexer::{Lexer, Token};
use crate::object::{Dictionary, ObjRef, Object, Stream};

/// Enrobe le lexer avec un buffer de lookahead : nécessaire pour distinguer
/// un entier isolé d'une référence indirecte `N G R` (trois tokens de suite).
pub struct Parser<'a> {
    lexer: Lexer<'a>,
    data: &'a [u8],
    buffered: Vec<Token>,
}

impl<'a> Parser<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            lexer: Lexer::new(data),
            data,
            buffered: Vec::new(),
        }
    }

    pub fn with_pos(data: &'a [u8], pos: usize) -> Self {
        Self {
            lexer: Lexer::with_pos(data, pos),
            data,
            buffered: Vec::new(),
        }
    }

    pub fn pos(&self) -> usize {
        if self.buffered.is_empty() {
            self.lexer.pos()
        } else {
            // Position imprécise quand des tokens sont bufferisés ; suffisant
            // pour les usages actuels (recherche de `stream` juste après un dict).
            self.lexer.pos()
        }
    }

    pub fn seek(&mut self, pos: usize) {
        self.buffered.clear();
        self.lexer.seek(pos);
    }

    fn next_token(&mut self) -> Result<Option<Token>> {
        if let Some(t) = self.buffered.pop() {
            return Ok(Some(t));
        }
        self.lexer.next_token()
    }

    fn peek_token(&mut self, ahead: usize) -> Result<Option<Token>> {
        while self.buffered.len() <= ahead {
            match self.lexer.next_token()? {
                Some(t) => self.buffered.insert(0, t),
                None => return Ok(None),
            }
        }
        Ok(self.buffered.get(self.buffered.len() - 1 - ahead).cloned())
    }

    fn push_back(&mut self, token: Token) {
        self.buffered.push(token);
    }

    /// Parse un objet PDF complet (valeur), en résolvant les références `N G R`
    /// et en consommant le flux binaire d'un `stream` le cas échéant.
    pub fn parse_object(&mut self) -> Result<Object> {
        let Some(token) = self.next_token()? else {
            return Err(PdfError::UnexpectedEof(self.lexer.pos()));
        };
        self.parse_object_from(token)
    }

    fn parse_object_from(&mut self, token: Token) -> Result<Object> {
        match token {
            Token::Integer(n) => self.maybe_reference(n),
            Token::Real(f) => Ok(Object::Real(f)),
            Token::LiteralString(s) | Token::HexString(s) => Ok(Object::String(s)),
            Token::Name(n) => Ok(Object::Name(n)),
            Token::ArrayStart => self.parse_array(),
            Token::DictStart => self.parse_dict_or_stream(),
            Token::Keyword(kw) => match kw.as_str() {
                "true" => Ok(Object::Boolean(true)),
                "false" => Ok(Object::Boolean(false)),
                "null" => Ok(Object::Null),
                other => Err(PdfError::InvalidObject(
                    self.lexer.pos(),
                    format!("unexpected keyword `{other}`"),
                )),
            },
            other => Err(PdfError::InvalidObject(
                self.lexer.pos(),
                format!("unexpected token {other:?}"),
            )),
        }
    }

    /// Un entier peut être : une valeur simple, ou le début de `N G R`
    /// (référence indirecte). Nécessite deux tokens de lookahead.
    fn maybe_reference(&mut self, num: i64) -> Result<Object> {
        if num < 0 {
            return Ok(Object::Integer(num));
        }
        let gen_tok = self.peek_token(0)?;
        if let Some(Token::Integer(gen)) = gen_tok {
            let kw_tok = self.peek_token(1)?;
            if let Some(Token::Keyword(ref kw)) = kw_tok {
                if kw == "R" {
                    self.next_token()?; // consume gen
                    self.next_token()?; // consume "R"
                    return Ok(Object::Reference(ObjRef::new(num as u32, gen as u16)));
                }
            }
        }
        Ok(Object::Integer(num))
    }

    fn parse_array(&mut self) -> Result<Object> {
        let mut items = Vec::new();
        loop {
            let Some(token) = self.next_token()? else {
                return Err(PdfError::UnexpectedEof(self.lexer.pos()));
            };
            if token == Token::ArrayEnd {
                break;
            }
            items.push(self.parse_object_from(token)?);
        }
        Ok(Object::Array(items))
    }

    fn parse_dict_or_stream(&mut self) -> Result<Object> {
        let mut dict = Dictionary::new();
        loop {
            let Some(token) = self.next_token()? else {
                return Err(PdfError::UnexpectedEof(self.lexer.pos()));
            };
            if token == Token::DictEnd {
                break;
            }
            let Token::Name(key) = token else {
                return Err(PdfError::InvalidObject(
                    self.lexer.pos(),
                    "dictionary key must be a Name".to_string(),
                ));
            };
            let value = self.parse_object()?;
            dict.insert(key, value);
        }

        // Un dictionnaire directement suivi de `stream` est un objet Stream.
        if let Some(Token::Keyword(kw)) = self.peek_token(0)? {
            if kw == "stream" {
                self.next_token()?; // consume "stream"
                return self.parse_stream_body(dict);
            }
        }
        Ok(Object::Dictionary(dict))
    }

    fn parse_stream_body(&mut self, dict: Dictionary) -> Result<Object> {
        // Après le mot-clé `stream`, la spec impose CRLF ou LF (pas CR seul)
        // puis les données brutes.
        self.buffered.clear();
        let mut pos = self.lexer.pos();
        if self.data.get(pos) == Some(&b'\r') {
            pos += 1;
        }
        if self.data.get(pos) == Some(&b'\n') {
            pos += 1;
        }

        let length = match dict.get("Length") {
            // Un `/Length` négatif est invalide (trouvé par `cargo fuzz`,
            // Sprint 56) : `*n as usize` l'enroulerait vers un nombre
            // énorme — traité comme absent plutôt que rejeter tout
            // l'objet, même repli que pour une référence indirecte
            // ci-dessous (recherche littérale de `endstream`).
            Some(Object::Integer(n)) if *n >= 0 => Some(*n as usize),
            _ => None, // Référence indirecte : résolue plus tard par le Document (Sprint 3-4).
        };

        let (raw_data, end_pos) = if let Some(len) = length {
            // `saturating_add` : défense en profondeur même pour un
            // `/Length` positif mais absurdement grand (proche de
            // `usize::MAX`), qui pourrait sinon faire déborder l'addition.
            let end = pos.saturating_add(len).min(self.data.len());
            (self.data[pos..end].to_vec(), end)
        } else {
            // Fallback : chercher "endstream" littéralement.
            match find_subslice(&self.data[pos..], b"endstream") {
                Some(rel) => (self.data[pos..pos + rel].to_vec(), pos + rel),
                None => return Err(PdfError::UnexpectedEof(pos)),
            }
        };

        self.lexer.seek(end_pos);
        // Consommer le mot-clé "endstream" (avec tolérance sur les espaces).
        match self.next_token()? {
            Some(Token::Keyword(kw)) if kw == "endstream" => {}
            other => {
                return Err(PdfError::InvalidObject(
                    end_pos,
                    format!("expected `endstream`, found {other:?}"),
                ))
            }
        }

        Ok(Object::Stream(Stream { dict, raw_data }))
    }

    /// Parse un objet indirect complet : `N G obj ... endobj`.
    /// Retourne `(num, gen, object)`.
    pub fn parse_indirect_object(&mut self) -> Result<(u32, u16, Object)> {
        let num = match self.next_token()? {
            Some(Token::Integer(n)) if n >= 0 => n as u32,
            other => {
                return Err(PdfError::InvalidObject(
                    self.lexer.pos(),
                    format!("expected object number, found {other:?}"),
                ))
            }
        };
        let gen = match self.next_token()? {
            Some(Token::Integer(n)) if n >= 0 => n as u16,
            other => {
                return Err(PdfError::InvalidObject(
                    self.lexer.pos(),
                    format!("expected generation number, found {other:?}"),
                ))
            }
        };
        match self.next_token()? {
            Some(Token::Keyword(kw)) if kw == "obj" => {}
            other => {
                return Err(PdfError::InvalidObject(
                    self.lexer.pos(),
                    format!("expected `obj`, found {other:?}"),
                ))
            }
        }

        let object = self.parse_object()?;

        match self.next_token()? {
            Some(Token::Keyword(kw)) if kw == "endobj" => {}
            // Tolérance : certains PDF omettent endobj avant le prochain objet.
            Some(other) => self.push_back(other),
            None => {}
        }

        Ok((num, gen, object))
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_dictionary() {
        let input = b"<< /Type /Catalog /Count 3 /Ratio 1.5 >>";
        let mut parser = Parser::new(input);
        let obj = parser.parse_object().unwrap();
        let dict = obj.as_dict().unwrap();
        assert_eq!(dict.get("Type").unwrap().as_name(), Some("Catalog"));
        assert_eq!(dict.get_int("Count").unwrap(), 3);
    }

    #[test]
    fn parses_reference_vs_plain_integers() {
        let input = b"[1 2 3 0 R 7]";
        let mut parser = Parser::new(input);
        let obj = parser.parse_object().unwrap();
        let items = obj.as_array().unwrap();
        assert_eq!(items[0], Object::Integer(1));
        assert_eq!(items[1], Object::Integer(2));
        assert_eq!(items[2], Object::Reference(ObjRef::new(3, 0)));
        assert_eq!(items[3], Object::Integer(7));
    }

    #[test]
    fn parses_indirect_object() {
        let input = b"12 0 obj\n<< /Length 5 >>\nendobj";
        let mut parser = Parser::new(input);
        let (num, gen, obj) = parser.parse_indirect_object().unwrap();
        assert_eq!(num, 12);
        assert_eq!(gen, 0);
        assert!(obj.as_dict().is_some());
    }

    #[test]
    fn parses_stream_with_length() {
        let input = b"1 0 obj\n<< /Length 11 >>\nstream\nHello World\nendstream\nendobj";
        let mut parser = Parser::new(input);
        let (_, _, obj) = parser.parse_indirect_object().unwrap();
        match obj {
            Object::Stream(s) => assert_eq!(s.raw_data, b"Hello World"),
            other => panic!("expected stream, got {other:?}"),
        }
    }

    /// Régression trouvée par `cargo fuzz` (`fuzz/fuzz_targets/parse_document.rs`,
    /// Sprint 56) : un `/Length` négatif faisait déborder `*n as usize`
    /// (enroulé vers un nombre énorme) puis `pos + len` (`attempt to add
    /// with overflow` sur un build avec assertions de débordement activées)
    /// — doit se replier sur la recherche littérale de `endstream` comme
    /// pour une référence indirecte non résolue, pas paniquer.
    #[test]
    fn negative_length_falls_back_to_endstream_search_instead_of_panicking() {
        let input = b"1 0 obj\n<< /Length -10 >>\nstream\nHello World\nendstream\nendobj";
        let mut parser = Parser::new(input);
        let (_, _, obj) = parser.parse_indirect_object().unwrap();
        match obj {
            // Repli "recherche littérale de `endstream`" : inclut le saut
            // de ligne qui précède le mot-clé, comme pour toute autre
            // référence indirecte non résolue (même comportement, pas une
            // particularité du cas négatif).
            Object::Stream(s) => assert_eq!(s.raw_data, b"Hello World\n"),
            other => panic!("expected stream, got {other:?}"),
        }
    }

    #[test]
    fn parses_nested_array_and_dict() {
        let input = b"<< /Kids [1 0 R 2 0 R] /Resources << /Font << >> >> >>";
        let mut parser = Parser::new(input);
        let obj = parser.parse_object().unwrap();
        let dict = obj.as_dict().unwrap();
        assert_eq!(dict.get("Kids").unwrap().as_array().unwrap().len(), 2);
    }
}
