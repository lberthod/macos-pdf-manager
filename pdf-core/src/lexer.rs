//! Lexer/tokenizer PDF — architecture.md §4.1.
//!
//! Découpe un flux d'octets brut en tokens. Volontairement tolérant : un PDF
//! malformé ne doit pas faire paniquer le lexer, seulement produire une erreur
//! localisée ou un token dégradé.

use crate::error::{PdfError, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Integer(i64),
    Real(f64),
    LiteralString(Vec<u8>),
    HexString(Vec<u8>),
    Name(String),
    ArrayStart,
    ArrayEnd,
    DictStart,
    DictEnd,
    /// Mot-clé brut : `obj`, `endobj`, `stream`, `endstream`, `R`, `true`,
    /// `false`, `null`, `xref`, `trailer`, `startxref`, `n`, `f`, etc.
    Keyword(String),
}

fn is_whitespace(b: u8) -> bool {
    matches!(b, b'\0' | b'\t' | b'\n' | 0x0C | b'\r' | b' ')
}

fn is_delimiter(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn is_regular(b: u8) -> bool {
    !is_whitespace(b) && !is_delimiter(b)
}

pub struct Lexer<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn with_pos(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn seek(&mut self, pos: usize) {
        self.pos = pos.min(self.data.len());
    }

    /// Octets restants à partir de la position courante (utile pour les
    /// scans bruts, p. ex. localiser `EI` pour une image inline).
    pub fn remaining(&self) -> &'a [u8] {
        &self.data[self.pos..]
    }

    fn peek_byte(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.peek_byte();
        if b.is_some() {
            self.pos += 1;
        }
        b
    }

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            match self.peek_byte() {
                Some(b) if is_whitespace(b) => {
                    self.pos += 1;
                }
                Some(b'%') => {
                    while let Some(b) = self.peek_byte() {
                        if b == b'\n' || b == b'\r' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                _ => break,
            }
        }
    }

    /// Retourne le prochain token, ou `None` en fin de flux.
    pub fn next_token(&mut self) -> Result<Option<Token>> {
        self.skip_whitespace_and_comments();
        let start = self.pos;
        let Some(b) = self.peek_byte() else {
            return Ok(None);
        };

        let token = match b {
            b'[' => {
                self.pos += 1;
                Token::ArrayStart
            }
            b']' => {
                self.pos += 1;
                Token::ArrayEnd
            }
            b'<' => {
                if self.data.get(self.pos + 1) == Some(&b'<') {
                    self.pos += 2;
                    Token::DictStart
                } else {
                    self.pos += 1;
                    Token::HexString(self.read_hex_string()?)
                }
            }
            b'>' if self.data.get(self.pos + 1) == Some(&b'>') => {
                self.pos += 2;
                Token::DictEnd
            }
            b'(' => {
                self.pos += 1;
                Token::LiteralString(self.read_literal_string()?)
            }
            b'/' => {
                self.pos += 1;
                Token::Name(self.read_name()?)
            }
            b'+' | b'-' | b'.' | b'0'..=b'9' => self.read_number()?,
            _ if is_regular(b) => Token::Keyword(self.read_keyword()),
            _ => {
                return Err(PdfError::UnexpectedByte {
                    byte: b,
                    offset: start,
                })
            }
        };
        Ok(Some(token))
    }

    fn read_keyword(&mut self) -> String {
        let start = self.pos;
        while let Some(b) = self.peek_byte() {
            if is_regular(b) {
                self.pos += 1;
            } else {
                break;
            }
        }
        String::from_utf8_lossy(&self.data[start..self.pos]).into_owned()
    }

    fn read_number(&mut self) -> Result<Token> {
        let start = self.pos;
        if matches!(self.peek_byte(), Some(b'+') | Some(b'-')) {
            self.pos += 1;
        }
        let mut is_real = false;
        while let Some(b) = self.peek_byte() {
            match b {
                b'0'..=b'9' => self.pos += 1,
                b'.' => {
                    is_real = true;
                    self.pos += 1;
                }
                _ => break,
            }
        }
        let text = std::str::from_utf8(&self.data[start..self.pos]).unwrap_or("0");
        if is_real {
            // Tolère les formats dégénérés type "12." ou ".5" ou "-.5".
            let normalized = if text.starts_with('.') {
                format!("0{text}")
            } else if text.starts_with("-.") {
                format!("-0{}", &text[1..])
            } else {
                text.to_string()
            };
            let normalized = normalized.trim_end_matches('.');
            let value: f64 = normalized.parse().unwrap_or(0.0);
            Ok(Token::Real(value))
        } else {
            match text.parse::<i64>() {
                Ok(v) => Ok(Token::Integer(v)),
                Err(_) => Ok(Token::Real(text.parse().unwrap_or(0.0))),
            }
        }
    }

    fn read_name(&mut self) -> Result<String> {
        let mut out = Vec::new();
        while let Some(b) = self.peek_byte() {
            if !is_regular(b) {
                break;
            }
            if b == b'#' {
                if let (Some(h1), Some(h2)) =
                    (self.data.get(self.pos + 1), self.data.get(self.pos + 2))
                {
                    if let (Some(d1), Some(d2)) = (hex_val(*h1), hex_val(*h2)) {
                        out.push(d1 * 16 + d2);
                        self.pos += 3;
                        continue;
                    }
                }
            }
            out.push(b);
            self.pos += 1;
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    fn read_literal_string(&mut self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut depth = 1;
        loop {
            let Some(b) = self.bump() else {
                return Err(PdfError::UnexpectedEof(self.pos));
            };
            match b {
                b'(' => {
                    depth += 1;
                    out.push(b);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push(b);
                }
                b'\\' => {
                    let Some(esc) = self.bump() else {
                        return Err(PdfError::UnexpectedEof(self.pos));
                    };
                    match esc {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0C),
                        b'(' => out.push(b'('),
                        b')' => out.push(b')'),
                        b'\\' => out.push(b'\\'),
                        b'\r' => {
                            // Continuation de ligne : \<CR> ou \<CR><LF> -> ignoré.
                            if self.peek_byte() == Some(b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}
                        b'0'..=b'7' => {
                            let mut value = (esc - b'0') as u32;
                            for _ in 0..2 {
                                match self.peek_byte() {
                                    Some(d @ b'0'..=b'7') => {
                                        value = value * 8 + (d - b'0') as u32;
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push((value & 0xFF) as u8);
                        }
                        other => out.push(other),
                    }
                }
                other => out.push(other),
            }
        }
        Ok(out)
    }

    fn read_hex_string(&mut self) -> Result<Vec<u8>> {
        let mut digits = Vec::new();
        loop {
            let Some(b) = self.bump() else {
                return Err(PdfError::UnexpectedEof(self.pos));
            };
            if b == b'>' {
                break;
            }
            if is_whitespace(b) {
                continue;
            }
            match hex_val(b) {
                Some(v) => digits.push(v),
                None => {
                    return Err(PdfError::UnexpectedByte {
                        byte: b,
                        offset: self.pos - 1,
                    })
                }
            }
        }
        if digits.len() % 2 == 1 {
            digits.push(0);
        }
        Ok(digits
            .chunks(2)
            .map(|pair| pair[0] * 16 + pair[1])
            .collect())
    }
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(input: &str) -> Vec<Token> {
        let mut lexer = Lexer::new(input.as_bytes());
        let mut out = Vec::new();
        while let Some(t) = lexer.next_token().unwrap() {
            out.push(t);
        }
        out
    }

    #[test]
    fn numbers() {
        assert_eq!(
            tokens("12 -7 3.25 -.5 4."),
            vec![
                Token::Integer(12),
                Token::Integer(-7),
                Token::Real(3.25),
                Token::Real(-0.5),
                Token::Real(4.0),
            ]
        );
    }

    #[test]
    fn names_with_escapes() {
        assert_eq!(
            tokens("/Name1 /A#42 /Two#20Words"),
            vec![
                Token::Name("Name1".into()),
                Token::Name("AB".into()),
                Token::Name("Two Words".into()),
            ]
        );
    }

    #[test]
    fn literal_string_escapes() {
        assert_eq!(
            tokens(r"(Hello\nWorld) (Nested (parens)) (Octal \101)"),
            vec![
                Token::LiteralString(b"Hello\nWorld".to_vec()),
                Token::LiteralString(b"Nested (parens)".to_vec()),
                Token::LiteralString(b"Octal A".to_vec()),
            ]
        );
    }

    #[test]
    fn hex_string() {
        assert_eq!(
            tokens("<48656C6C6F> <48656C6C>"),
            vec![
                Token::HexString(b"Hello".to_vec()),
                Token::HexString(b"Hell".to_vec()),
            ]
        );
    }

    #[test]
    fn dict_and_array_delimiters() {
        assert_eq!(
            tokens("<< /Type /Catalog >> [1 2 3]"),
            vec![
                Token::DictStart,
                Token::Name("Type".into()),
                Token::Name("Catalog".into()),
                Token::DictEnd,
                Token::ArrayStart,
                Token::Integer(1),
                Token::Integer(2),
                Token::Integer(3),
                Token::ArrayEnd,
            ]
        );
    }

    #[test]
    fn keywords() {
        assert_eq!(
            tokens("12 0 obj true false null endobj"),
            vec![
                Token::Integer(12),
                Token::Integer(0),
                Token::Keyword("obj".into()),
                Token::Keyword("true".into()),
                Token::Keyword("false".into()),
                Token::Keyword("null".into()),
                Token::Keyword("endobj".into()),
            ]
        );
    }

    #[test]
    fn comments_are_skipped() {
        assert_eq!(
            tokens("1 % a comment\n2"),
            vec![Token::Integer(1), Token::Integer(2)]
        );
    }
}
