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
//! à 8 bits par composante. Canal alpha via `/SMask` (masque de fondu en
//! niveaux de gris, ISO 32000-1 §11.6.5.3) : décodé récursivement comme une
//! image `DeviceGray` à part entière, puis rééchantillonné au plus proche
//! voisin si ses dimensions diffèrent de l'image principale (rare, mais
//! légal selon la spec).
//!
//! Non supporté (voir sprint.md) : `CCITTFaxDecode`, `JBIG2Decode`,
//! `JPXDecode`, espaces `Indexed`/`Separation`/`Lab`, profondeurs 1/2/4/16
//! bits, `/Mask` (masque de détourage 1 bit ou par plage de couleurs,
//! différent de `/SMask`), `/ImageMask`.

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

    // Plafond de sanité (AUDIT_FIXES_2026-09-03.md #11) : `/Width`/`/Height`
    // forgés (ex. 100000×100000) feraient sinon tenter une allocation de
    // plusieurs Go pour `rgba` ci-dessous à partir d'un flux minuscule —
    // `checked_mul` en plus, `width as u64 * height as u64` peut à la limite
    // approcher `u64::MAX` pour des `/Width`/`/Height` proches d'`u32::MAX`.
    const MAX_IMAGE_PIXELS: u64 = 64_000_000;
    let pixel_count_u64 = (width as u64)
        .checked_mul(height as u64)
        .ok_or_else(|| PdfError::DecodeError("image dimensions overflow".to_string()))?;
    if pixel_count_u64 > MAX_IMAGE_PIXELS {
        return Err(PdfError::DecodeError(format!(
            "image too large: {width}x{height} exceeds the {MAX_IMAGE_PIXELS} pixel limit"
        )));
    }

    let declared_color_space = resolve_color_space(doc, &stream.dict)?;

    let decoded = decode_stream(stream)?;

    // `zune-jpeg` (comme de nombreux décodeurs JPEG, y compris ceux des
    // navigateurs) convertit certains JPEG CMYK/YCCK en sortie RGB à 3
    // composantes plutôt que de préserver les 4 composantes d'origine —
    // observé sur des CMYK JPEG produits par Pillow. Dans ce cas, la
    // disposition réelle des octets décodés ne correspond plus au
    // `/ColorSpace` déclaré (`DeviceCMYK`) : on fait confiance à ce que le
    // décodeur a effectivement produit plutôt qu'à la déclaration PDF.
    let pixel_count = pixel_count_u64 as usize;
    let color_space = if declared_color_space == ColorSpaceKind::Cmyk
        && decoded.len() >= pixel_count * 3
        && decoded.len() < pixel_count * 4
    {
        ColorSpaceKind::Rgb
    } else {
        declared_color_space
    };
    let components = color_space.components();

    let expected_len = pixel_count * components;
    if decoded.len() < expected_len {
        return Err(PdfError::DecodeError(format!(
            "image data too short: got {} bytes, expected at least {expected_len}",
            decoded.len()
        )));
    }

    let mut rgba = Vec::with_capacity(pixel_count * 4);
    for pixel in decoded[..expected_len].chunks_exact(components) {
        let (r, g, b) = match color_space {
            ColorSpaceKind::Gray => (pixel[0], pixel[0], pixel[0]),
            ColorSpaceKind::Rgb => (pixel[0], pixel[1], pixel[2]),
            ColorSpaceKind::Cmyk => cmyk_to_rgb(pixel[0], pixel[1], pixel[2], pixel[3]),
        };
        rgba.extend_from_slice(&[r, g, b, 255]);
    }

    if let Some(smask_obj) = stream.dict.get("SMask") {
        if let Ok(Object::Stream(smask_stream)) = doc.get(smask_obj) {
            if let Ok(smask) = decode_image(doc, &smask_stream) {
                apply_soft_mask(&mut rgba, width, height, &smask);
            }
        }
    }

    Ok(DecodedImage {
        width,
        height,
        rgba,
    })
}

/// Applique un masque de fondu (`/SMask`) déjà décodé comme canal alpha,
/// en rééchantillonnant au plus proche voisin si ses dimensions diffèrent
/// de l'image principale. Le niveau de gris (canal R, puisque R=G=B pour
/// une image `DeviceGray` décodée) devient directement la valeur alpha.
fn apply_soft_mask(rgba: &mut [u8], width: u32, height: u32, smask: &DecodedImage) {
    for y in 0..height {
        let sy = if height == smask.height {
            y
        } else {
            (y as u64 * smask.height as u64 / height as u64) as u32
        };
        for x in 0..width {
            let sx = if width == smask.width {
                x
            } else {
                (x as u64 * smask.width as u64 / width as u64) as u32
            };
            let src_idx = (sy as usize * smask.width as usize + sx as usize) * 4;
            let Some(&alpha) = smask.rgba.get(src_idx) else {
                continue;
            };
            let dst_idx = (y as usize * width as usize + x as usize) * 4 + 3;
            rgba[dst_idx] = alpha;
        }
    }
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

    fn empty_doc() -> Document {
        let body = concat!(
            "%PDF-1.7\n",
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
            "2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n",
        );
        let mut bytes = body.as_bytes().to_vec();
        let off1 = 9;
        let off2 = bytes.windows(7).position(|w| w == b"2 0 obj").unwrap();
        let xref_offset = bytes.len();
        let xref = format!(
            "xref\n0 3\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \ntrailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF"
        );
        bytes.extend_from_slice(xref.as_bytes());
        Document::open(bytes).unwrap()
    }

    /// AUDIT_FIXES_2026-09-03.md #11 : une `/Width`/`/Height` forgée
    /// (100000×100000) doit être rejetée avant de tenter d'allouer le bitmap
    /// RGBA correspondant (40 Go), pas seulement échouer plus tard faute de
    /// données suffisantes dans le flux.
    #[test]
    fn decode_image_rejects_absurd_dimensions_instead_of_allocating_gigabytes() {
        let doc = empty_doc();
        let mut dict = Dictionary::new();
        dict.insert("Type", Object::Name("XObject".to_string()));
        dict.insert("Subtype", Object::Name("Image".to_string()));
        dict.insert("Width", Object::Integer(100_000));
        dict.insert("Height", Object::Integer(100_000));
        dict.insert("BitsPerComponent", Object::Integer(8));
        dict.insert("ColorSpace", Object::Name("DeviceRGB".to_string()));
        let stream = Stream {
            dict,
            raw_data: Vec::new(),
        };

        let err = decode_image(&doc, &stream).unwrap_err();
        assert!(matches!(err, PdfError::DecodeError(_)));
    }

    #[test]
    fn soft_mask_same_size_sets_alpha_directly() {
        // Image principale 2x1, opaque (alpha=255) au départ.
        let mut rgba = vec![255, 0, 0, 255, 0, 255, 0, 255];
        let smask = DecodedImage {
            width: 2,
            height: 1,
            // Gris décodé : R=G=B=valeur. Pixel 0 opaque, pixel 1 transparent.
            rgba: vec![255, 255, 255, 255, 0, 0, 0, 255],
        };
        apply_soft_mask(&mut rgba, 2, 1, &smask);
        assert_eq!(rgba, vec![255, 0, 0, 255, 0, 255, 0, 0]);
    }

    #[test]
    fn soft_mask_smaller_size_is_upsampled_nearest_neighbor() {
        // Image principale 4x1, masque 2x1 (moitié gauche opaque, droite transparente).
        let mut rgba = [10, 10, 10, 255].repeat(4);
        let smask = DecodedImage {
            width: 2,
            height: 1,
            rgba: vec![255, 255, 255, 255, 0, 0, 0, 255],
        };
        apply_soft_mask(&mut rgba, 4, 1, &smask);
        let alphas: Vec<u8> = rgba.chunks(4).map(|p| p[3]).collect();
        assert_eq!(alphas, vec![255, 255, 0, 0]);
    }
}
