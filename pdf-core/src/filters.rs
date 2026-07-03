//! Filtres de décodage de flux — architecture.md §4.3.
//!
//! Implémentés : `FlateDecode`, `ASCIIHexDecode`, `ASCII85Decode`, `LZWDecode`,
//! plus les prédicteurs PNG/TIFF appliqués en aval (nécessaires pour décoder
//! la plupart des cross-reference streams et object streams, PDF 1.5+).
//! Restent à faire : `CCITTFaxDecode`, `JBIG2Decode`, `JPXDecode` (images,
//! priorité basse — voir sprint.md).

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

/// Retourne le dictionnaire `/DecodeParms` correspondant au filtre d'indice
/// `index` (parmi potentiellement plusieurs filtres chaînés).
fn decode_parms_at(dict: &Dictionary, index: usize) -> Option<Dictionary> {
    let parms = dict.get("DecodeParms").or_else(|| dict.get("DP"))?;
    match parms {
        Object::Dictionary(d) => (index == 0).then(|| d.clone()),
        Object::Array(items) => items.get(index).and_then(|o| o.as_dict().cloned()),
        _ => None,
    }
}

/// Applique la chaîne de filtres déclarée par `/Filter` sur les données brutes.
pub fn decode_stream(stream: &Stream) -> Result<Vec<u8>> {
    let mut data = stream.raw_data.clone();
    for (index, name) in filter_names(&stream.dict).into_iter().enumerate() {
        data = match name.as_str() {
            "FlateDecode" | "Fl" => flate_decode(&data)?,
            "ASCIIHexDecode" | "AHx" => ascii_hex_decode(&data)?,
            "ASCII85Decode" | "A85" => ascii85_decode(&data)?,
            "LZWDecode" | "LZW" => lzw_decode(&data)?,
            other => return Err(PdfError::UnsupportedFilter(other.to_string())),
        };
        if let Some(parms) = decode_parms_at(&stream.dict, index) {
            data = apply_predictor(&data, &parms)?;
        }
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

/// Décodage LZW tel qu'utilisé par PDF (variante TIFF, code clear=256,
/// eod=257, largeur de code variable 9-12 bits, "early change" activé par
/// défaut — architecture.md §4.3).
fn lzw_decode(data: &[u8]) -> Result<Vec<u8>> {
    const CLEAR: u16 = 256;
    const EOD: u16 = 257;

    fn fresh_table() -> Vec<Vec<u8>> {
        (0..256u16).map(|i| vec![i as u8]).collect()
    }

    let mut out = Vec::new();
    let mut table = fresh_table();
    let mut code_width = 9u32;
    let mut prev_code: Option<u16> = None;
    let mut bit_pos = 0usize;
    let total_bits = data.len() * 8;

    loop {
        if bit_pos + code_width as usize > total_bits {
            break;
        }
        let code = read_bits(data, bit_pos, code_width);
        bit_pos += code_width as usize;

        if code == CLEAR {
            table = fresh_table();
            code_width = 9;
            prev_code = None;
            continue;
        }
        if code == EOD {
            break;
        }

        let entry: Vec<u8> = if (code as usize) < table.len() {
            table[code as usize].clone()
        } else if code as usize == table.len() {
            // Cas KwKwK : l'entrée n'existe pas encore, elle vaut prev + prev[0].
            let prev = prev_code
                .and_then(|c| table.get(c as usize))
                .ok_or_else(|| PdfError::DecodeError("LZWDecode: invalid code sequence".into()))?;
            let mut e = prev.clone();
            e.push(prev[0]);
            e
        } else {
            return Err(PdfError::DecodeError(format!(
                "LZWDecode: invalid code {code}"
            )));
        };

        out.extend_from_slice(&entry);

        if let Some(p) = prev_code {
            let mut new_entry = table[p as usize].clone();
            new_entry.push(entry[0]);
            table.push(new_entry);
            // "Early change" (comportement par défaut PDF) : la largeur de
            // code augmente un cran avant que la table n'atteigne la limite.
            match table.len() {
                511 => code_width = 10,
                1023 => code_width = 11,
                2047 => code_width = 12,
                _ => {}
            }
        }
        prev_code = Some(code);
    }

    Ok(out)
}

fn read_bits(data: &[u8], bit_pos: usize, width: u32) -> u16 {
    let mut value: u32 = 0;
    for i in 0..width as usize {
        let bit_index = bit_pos + i;
        let byte = data[bit_index / 8];
        let bit = (byte >> (7 - (bit_index % 8))) & 1;
        value = (value << 1) | bit as u32;
    }
    value as u16
}

/// Prédicteurs PNG (10-15) et TIFF (2) — architecture.md §4.3. `/Predictor`
/// vaut 1 (aucun) par défaut.
fn apply_predictor(data: &[u8], parms: &Dictionary) -> Result<Vec<u8>> {
    let predictor = parms.get("Predictor").and_then(|o| o.as_int()).unwrap_or(1);
    if predictor <= 1 {
        return Ok(data.to_vec());
    }
    let colors = parms.get("Colors").and_then(|o| o.as_int()).unwrap_or(1) as usize;
    let bpc = parms
        .get("BitsPerComponent")
        .and_then(|o| o.as_int())
        .unwrap_or(8) as usize;
    let columns = parms.get("Columns").and_then(|o| o.as_int()).unwrap_or(1) as usize;

    let bits_per_pixel = colors * bpc;
    let bytes_per_pixel = bits_per_pixel.div_ceil(8).max(1);
    let row_bytes = (columns * bits_per_pixel).div_ceil(8);

    if predictor == 2 {
        return Ok(tiff_predictor(data, row_bytes, bytes_per_pixel));
    }

    // PNG predictors : chaque ligne est précédée d'un octet de type (0-4).
    png_predictor(data, row_bytes, bytes_per_pixel)
}

fn tiff_predictor(data: &[u8], row_bytes: usize, bytes_per_pixel: usize) -> Vec<u8> {
    let mut out = data.to_vec();
    for row in out.chunks_mut(row_bytes) {
        for i in bytes_per_pixel..row.len() {
            row[i] = row[i].wrapping_add(row[i - bytes_per_pixel]);
        }
    }
    out
}

fn png_predictor(data: &[u8], row_bytes: usize, bytes_per_pixel: usize) -> Result<Vec<u8>> {
    let stride = row_bytes + 1; // +1 pour l'octet de type de filtre PNG
    if stride == 0 || !data.len().is_multiple_of(stride) {
        return Err(PdfError::DecodeError(
            "PNG predictor: data length is not a multiple of row stride".into(),
        ));
    }
    let mut out = Vec::with_capacity(data.len() / stride * row_bytes);
    let mut prev_row = vec![0u8; row_bytes];

    for chunk in data.chunks(stride) {
        let filter_type = chunk[0];
        let mut row = chunk[1..].to_vec();
        for i in 0..row.len() {
            let a = if i >= bytes_per_pixel {
                row[i - bytes_per_pixel]
            } else {
                0
            };
            let b = prev_row[i];
            let c = if i >= bytes_per_pixel {
                prev_row[i - bytes_per_pixel]
            } else {
                0
            };
            let recon = match filter_type {
                0 => row[i],
                1 => row[i].wrapping_add(a),
                2 => row[i].wrapping_add(b),
                3 => row[i].wrapping_add(((a as u16 + b as u16) / 2) as u8),
                4 => row[i].wrapping_add(paeth(a, b, c)),
                other => {
                    return Err(PdfError::DecodeError(format!(
                        "PNG predictor: unknown filter type {other}"
                    )))
                }
            };
            row[i] = recon;
        }
        out.extend_from_slice(&row);
        prev_row = row;
    }
    Ok(out)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (a, b, c) = (a as i32, b as i32, c as i32);
    let p = a + b - c;
    let pa = (p - a).abs();
    let pb = (p - b).abs();
    let pc = (p - c).abs();
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
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

    #[test]
    fn png_predictor_none_filter_is_identity() {
        // 2 lignes de 3 octets, filtre "None" (0) partout.
        let data = [0u8, 1, 2, 3, 0, 4, 5, 6];
        let decoded = png_predictor(&data, 3, 1).unwrap();
        assert_eq!(decoded, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn png_predictor_up_filter() {
        // ligne 1 (None): [1,2,3] ; ligne 2 (Up=2): delta par rapport à ligne 1.
        let data = [0u8, 1, 2, 3, 2, 1, 1, 1];
        let decoded = png_predictor(&data, 3, 1).unwrap();
        assert_eq!(decoded, vec![1, 2, 3, 2, 3, 4]);
    }

    #[test]
    fn tiff_predictor_roundtrip() {
        // 1 composant, 1 octet/pixel, delta appliqué par rapport au pixel précédent.
        let data = [10u8, 5, 5, 5]; // valeurs réelles: 10,15,20,25
        let decoded = tiff_predictor(&data, 4, 1);
        assert_eq!(decoded, vec![10, 15, 20, 25]);
    }

    #[test]
    fn lzw_roundtrip_ascii() {
        // Suite de codes LZW valide pour "-----A---B" (exemple classique de la spec PDF, Annexe D).
        // On teste plus simplement l'auto-cohérence : encode à la main un message très répétitif
        // n'est pas trivial sans encodeur ; on vérifie donc le cas trivial "pas de compression"
        // où chaque caractère reste un code littéral suivi d'EOD.
        // Codes: 'A'=65 sur 9 bits, puis EOD=257 sur 9 bits.
        let mut bits = String::new();
        bits.push_str(&format!("{:09b}", 65u16));
        bits.push_str(&format!("{:09b}", 257u16));
        let bytes = bits_to_bytes(&bits);
        let decoded = lzw_decode(&bytes).unwrap();
        assert_eq!(decoded, vec![65]);
    }

    fn bits_to_bytes(bits: &str) -> Vec<u8> {
        let mut bits = bits.to_string();
        while !bits.len().is_multiple_of(8) {
            bits.push('0');
        }
        bits.as_bytes()
            .chunks(8)
            .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 2).unwrap())
            .collect()
    }
}
