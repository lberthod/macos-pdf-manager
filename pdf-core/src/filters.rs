//! Filtres de décodage de flux — architecture.md §4.3.
//!
//! Seul `FlateDecode` est implémenté pour l'instant (couvre ~90% des flux
//! rencontrés en pratique). Les autres filtres (LZW, DCT, CCITT, JBIG2,
//! JPX) sont prévus pour des sprints ultérieurs — voir sprint.md.

use crate::error::{PdfError, Result};
use crate::object::{Dictionary, Object, Stream};
use flate2::read::ZlibDecoder;
use std::io::Read;

fn filter_names(dict: &Dictionary) -> Vec<String> {
    match dict.get("Filter") {
        Some(Object::Name(n)) => vec![n.clone()],
        Some(Object::Array(items)) => items
            .iter()
            .filter_map(|o| o.as_name().map(String::from))
            .collect(),
        _ => Vec::new(),
    }
}

/// Applique la chaîne de filtres déclarée par `/Filter` sur les données brutes.
pub fn decode_stream(stream: &Stream) -> Result<Vec<u8>> {
    let mut data = stream.raw_data.clone();
    for name in filter_names(&stream.dict) {
        data = match name.as_str() {
            "FlateDecode" | "Fl" => flate_decode(&data)?,
            "ASCIIHexDecode" | "AHx" => ascii_hex_decode(&data)?,
            "ASCII85Decode" | "A85" => ascii85_decode(&data)?,
            other => return Err(PdfError::UnsupportedFilter(other.to_string())),
        };
    }
    Ok(data)
}

fn flate_decode(data: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = ZlibDecoder::new(data);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|e| PdfError::DecodeError(format!("FlateDecode: {e}")))?;
    Ok(out)
}

fn ascii_hex_decode(data: &[u8]) -> Result<Vec<u8>> {
    let mut digits = Vec::new();
    for &b in data {
        if b == b'>' {
            break;
        }
        if b.is_ascii_whitespace() {
            continue;
        }
        let v = (b as char)
            .to_digit(16)
            .ok_or_else(|| PdfError::DecodeError("ASCIIHexDecode: invalid digit".into()))?;
        digits.push(v as u8);
    }
    if digits.len() % 2 == 1 {
        digits.push(0);
    }
    Ok(digits.chunks(2).map(|c| c[0] * 16 + c[1]).collect())
}

fn ascii85_decode(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut group = [0u8; 5];
    let mut count = 0;
    let mut iter = data.iter().copied();

    // Ignore un éventuel préfixe "<~".
    if data.starts_with(b"<~") {
        iter.next();
        iter.next();
    }

    for b in iter {
        if b == b'~' {
            break;
        }
        if b.is_ascii_whitespace() {
            continue;
        }
        if b == b'z' && count == 0 {
            out.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        group[count] = b - 33;
        count += 1;
        if count == 5 {
            out.extend_from_slice(&decode_group85(&group, 4));
            count = 0;
        }
    }
    if count > 0 {
        for slot in group.iter_mut().skip(count) {
            *slot = 84; // padding avec 'u'-33
        }
        let decoded = decode_group85(&group, count - 1);
        out.extend_from_slice(&decoded);
    }
    Ok(out)
}

fn decode_group85(group: &[u8; 5], out_len: usize) -> Vec<u8> {
    let mut value: u32 = 0;
    for &g in group {
        value = value.wrapping_mul(85).wrapping_add(g as u32);
    }
    let bytes = value.to_be_bytes();
    bytes[..out_len].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    #[test]
    fn flate_roundtrip() {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(b"Hello, PDF!").unwrap();
        let compressed = encoder.finish().unwrap();
        let decoded = flate_decode(&compressed).unwrap();
        assert_eq!(decoded, b"Hello, PDF!");
    }

    #[test]
    fn ascii_hex_roundtrip() {
        let decoded = ascii_hex_decode(b"48656C6C6F>").unwrap();
        assert_eq!(decoded, b"Hello");
    }

    #[test]
    fn ascii85_roundtrip() {
        let decoded = ascii85_decode(b"87cURD_*#4DfTZ)+T~>").unwrap();
        assert_eq!(decoded, b"Hello, World!");
    }
}
