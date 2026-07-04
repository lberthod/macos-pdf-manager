//! Décodage d'images PDF (XObjects `/Subtype /Image`) en pixels RGBA8 —
//! architecture.md §4.3 (filtres) et §5 (rendu).
//!
//! `filters::decode_stream` fait le gros du travail de décompression
//! (Flate/LZW/DCTDecode) ; ce module interprète le résultat — une suite
//! d'échantillons entrelacés — à la lumière de `/ColorSpace` et
//! `/BitsPerComponent` pour produire un bitmap RGBA8 directement utilisable
//! par `pdf-render`.
//!
//! Supporté : `DeviceGray`/`DeviceRGB`/`DeviceCMYK` (+ `CalGray`/`CalRGB`
//! traités comme leurs équivalents `Device*`) et `ICCBased` (approximé par
//! son nombre de composantes `/N`, sans tenir compte du profil ICC lui-même),
//! à 8 bits par composante.
//!
//! Non supporté (voir sprint.md) : `CCITTFaxDecode`, `JBIG2Decode`,
//! `JPXDecode`, espaces `Indexed`/`Separation`/`Lab`, profondeurs 1/2/4/16
//! bits, canal alpha (`/SMask`, `/Mask`), `/ImageMask`.

use crate::display::DecodedImage;
use crate::document::Document;
use crate::error::{PdfError, Result};
use crate::filters::decode_stream;
use crate::object::{Dictionary, Object, Stream};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColorSpaceKind {
    Gray,
    Rgb,
    Cmyk,
}

impl ColorSpaceKind {
    fn components(self) -> usize {
        match self {
            ColorSpaceKind::Gray => 1,
            ColorSpaceKind::Rgb => 3,
            ColorSpaceKind::Cmyk => 4,
        }
    }
}

/// Décode une image XObject en bitmap RGBA8. Retourne une erreur explicite
/// (jamais de panique) si le format n'est pas supporté ; l'appelant
/// (`interp.rs`) traite ça comme une absence de pixels plutôt qu'un échec
/// fatal de l'interprétation du reste de la page.
pub fn decode_image(doc: &Document, stream: &Stream) -> Result<DecodedImage> {
    let width = stream.dict.get_int("Width")?.max(0) as u32;
    let height = stream.dict.get_int("Height")?.max(0) as u32;
    if width == 0 || height == 0 {
        return Err(PdfError::DecodeError("image has zero width/height".into()));
    }

    let bpc = stream
        .dict
        .get("BitsPerComponent")
        .and_then(|o| o.as_int())
        .unwrap_or(8);
    if bpc != 8 {
        return Err(PdfError::UnsupportedFilter(format!(
            "{bpc}-bit-per-component images are not supported yet"
        )));
    }

    let color_space = resolve_color_space(doc, &stream.dict)?;
    let components = color_space.components();

    let decoded = decode_stream(stream)?;
    let expected_len = width as usize * height as usize * components;
    if decoded.len() < expected_len {
        return Err(PdfError::DecodeError(format!(
            "image data too short: got {} bytes, expected at least {expected_len}",
            decoded.len()
        )));
    }

    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for pixel in decoded[..expected_len].chunks_exact(components) {
        let (r, g, b) = match color_space {
            ColorSpaceKind::Gray => (pixel[0], pixel[0], pixel[0]),
            ColorSpaceKind::Rgb => (pixel[0], pixel[1], pixel[2]),
            ColorSpaceKind::Cmyk => cmyk_to_rgb(pixel[0], pixel[1], pixel[2], pixel[3]),
        };
        rgba.extend_from_slice(&[r, g, b, 255]);
    }

    Ok(DecodedImage {
        width,
        height,
        rgba,
    })
}

fn cmyk_to_rgb(c: u8, m: u8, y: u8, k: u8) -> (u8, u8, u8) {
    // Conversion naïve, sans profil ICC (identique à celle utilisée pour les
    // couleurs de remplissage/trait en CMYK, voir interp.rs/pdf-render).
    let (c, m, y, k) = (
        c as f32 / 255.0,
        m as f32 / 255.0,
        y as f32 / 255.0,
        k as f32 / 255.0,
    );
    let r = (1.0 - c) * (1.0 - k);
    let g = (1.0 - m) * (1.0 - k);
    let b = (1.0 - y) * (1.0 - k);
    (
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
    )
}

fn resolve_color_space(doc: &Document, dict: &Dictionary) -> Result<ColorSpaceKind> {
    let cs_obj = dict
        .get("ColorSpace")
        .or_else(|| dict.get("CS"))
        .ok_or_else(|| PdfError::MissingKey("ColorSpace".into()))?;
    let cs = doc.get(cs_obj)?;

    match &cs {
        Object::Name(name) => match name.as_str() {
            "DeviceGray" | "CalGray" | "G" => Ok(ColorSpaceKind::Gray),
            "DeviceRGB" | "CalRGB" | "RGB" => Ok(ColorSpaceKind::Rgb),
            "DeviceCMYK" | "CMYK" => Ok(ColorSpaceKind::Cmyk),
            other => Err(PdfError::UnsupportedFilter(format!(
                "color space /{other} is not supported"
            ))),
        },
        Object::Array(items) => {
            let family = items.first().and_then(|o| o.as_name()).unwrap_or("");
            match family {
                "ICCBased" => {
                    let icc_stream = items
                        .get(1)
                        .map(|o| doc.get(o))
                        .transpose()?
                        .ok_or_else(|| PdfError::MissingKey("ICCBased stream".into()))?;
                    let n = icc_stream
                        .as_dict()
                        .and_then(|d| d.get_int("N").ok())
                        .unwrap_or(3);
                    match n {
                        1 => Ok(ColorSpaceKind::Gray),
                        4 => Ok(ColorSpaceKind::Cmyk),
                        _ => Ok(ColorSpaceKind::Rgb),
                    }
                }
                other => Err(PdfError::UnsupportedFilter(format!(
                    "color space family /{other} is not supported"
                ))),
            }
        }
        _ => Err(PdfError::UnexpectedType("Name or Array for /ColorSpace")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmyk_black_is_rgb_black_and_white_is_white() {
        assert_eq!(cmyk_to_rgb(0, 0, 0, 255), (0, 0, 0));
        assert_eq!(cmyk_to_rgb(0, 0, 0, 0), (255, 255, 255));
    }

    #[test]
    fn color_space_kind_component_counts() {
        assert_eq!(ColorSpaceKind::Gray.components(), 1);
        assert_eq!(ColorSpaceKind::Rgb.components(), 3);
        assert_eq!(ColorSpaceKind::Cmyk.components(), 4);
    }
}
