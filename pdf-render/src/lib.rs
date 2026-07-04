//! Rasterisation CPU d'une `DisplayList` via `tiny-skia` — architecture.md
//! §5. Back-end de référence : simple, déterministe, sert de base aux
//! futurs tests de non-régression pixel (corpus + harnais de diff, prévus
//! Sprint 0/2 mais pas encore constitués — voir sprint.md).
//!
//! Limitations connues à ce stade :
//! - Les glyphes ne sont dessinés que lorsque `DisplayItem::Glyph::outline`
//!   est renseigné (police TrueType intégrée résolue, ou substitution
//!   système — voir `pdf-core::font`). Les polices CFF/Type1 intégrées
//!   n'ont pas encore de contour disponible.
//! - Les images ne sont dessinées que lorsque `DisplayItem::Image::pixels`
//!   est renseigné (voir `pdf-core::image` pour les formats supportés :
//!   `DCTDecode`/JPEG et échantillons bruts 8 bits DeviceGray/RGB/CMYK,
//!   canal alpha via `/SMask` prémultiplié ici avant rendu ; pas de
//!   CCITT/JBIG2/JPX).
//! - Le clip (`W`/`W*`, porté par `DisplayItem::*::clip`) est appliqué via un
//!   masque `tiny_skia::Mask` rastérisé par intersection des clips imbriqués.
//! - Espaces colorimétriques : conversion CMYK -> RGB naïve (sans profil ICC).
//! - La rotation de page (`/Rotate`, voir `render_page_rotated`) est
//!   appliquée en aval de la rasterisation via une matrice, pas nativement
//!   par tiny-skia.

use std::rc::Rc;

use pdf_core::display::{
    ClipStack, Color, DecodedImage, DisplayItem, DisplayList, FillRule, Matrix, PaintOp,
    PathSegment,
};
use tiny_skia::{
    Color as SkiaColor, FillRule as SkiaFillRule, Mask, Paint, Path, PathBuilder, Pixmap,
    PixmapPaint, PixmapRef, Stroke, Transform,
};

/// Rasterise une page entière en se basant sur son `MediaBox`
/// (`[x0, y0, x1, y1]`, ISO 32000-1 §7.7.3.3) : la taille du pixmap suit la
/// largeur/hauteur du MediaBox en points PDF (1 pixel = 1 point). Équivalent
/// à `render_page_scaled(display, media_box, 1.0)`. N'applique pas
/// `/Rotate` — voir `render_page_rotated` pour ça.
pub fn render_page(display: &DisplayList, media_box: [f64; 4]) -> Option<Pixmap> {
    render_page_scaled(display, media_box, 1.0)
}

/// Comme `render_page`, avec un facteur d'échelle (zoom) : `scale = 2.0`
/// produit un pixmap deux fois plus grand, re-rasterisé à cette résolution
/// (pas un agrandissement d'image a posteriori — plus net à l'écran).
pub fn render_page_scaled(
    display: &DisplayList,
    media_box: [f64; 4],
    scale: f64,
) -> Option<Pixmap> {
    render_page_rotated(display, media_box, 0, scale)
}

/// Comme `render_page_scaled`, en appliquant en plus la rotation de page
/// (`/Rotate`, ISO 32000-1 §7.7.3.3 : multiple de 90° dans le sens horaire,
/// vu du lecteur). Les dimensions du pixmap sont permutées pour 90°/270°
/// (portrait -> paysage). `rotate` est normalisé au multiple de 90 le plus
/// proche modulo 360 (robustesse face à une valeur non conforme).
pub fn render_page_rotated(
    display: &DisplayList,
    media_box: [f64; 4],
    rotate: i32,
    scale: f64,
) -> Option<Pixmap> {
    let scale = scale.max(0.01);
    let rotate = normalize_rotate(rotate);
    let unrotated_w = (media_box[2] - media_box[0]) * scale;
    let unrotated_h = (media_box[3] - media_box[1]) * scale;
    let (out_w, out_h) = if rotate == 90 || rotate == 270 {
        (unrotated_h, unrotated_w)
    } else {
        (unrotated_w, unrotated_h)
    };
    let width = out_w.round().max(1.0) as u32;
    let height = out_h.round().max(1.0) as u32;

    // Espace utilisateur PDF (origine bas-gauche) -> espace "device" non
    // pivoté (origine haut-gauche, taille de page) -> espace pixmap final
    // (dimensions permutées si 90°/270°, voir `rotation_matrix`).
    let device = page_flip_matrix(media_box[0], media_box[3], scale).then(&rotation_matrix(
        rotate,
        unrotated_w,
        unrotated_h,
    ));

    render_to_pixmap(display, width, height, &device, scale)
}

/// Normalise une valeur `/Rotate` arbitraire au multiple de 90 le plus
/// proche dans `{0, 90, 180, 270}` (le spec impose un multiple de 90, mais
/// un PDF malformé pourrait contenir autre chose).
fn normalize_rotate(rotate: i32) -> i32 {
    let r = rotate.rem_euclid(360);
    match r {
        315..=359 | 0..=44 => 0,
        45..=134 => 90,
        135..=224 => 180,
        _ => 270,
    }
}

/// Transforme les coordonnées de l'espace "device" non pivoté (taille de
/// page, origine haut-gauche) vers l'espace pixmap final, pour un `rotate`
/// déjà normalisé (0/90/180/270). Dérivée en considérant que pivoter la
/// page dans le sens horaire déplace son coin haut-gauche vers le coin
/// haut-droit du canevas final (pour 90°), etc.
fn rotation_matrix(rotate: i32, unrotated_w: f64, unrotated_h: f64) -> Matrix {
    match rotate {
        90 => Matrix::new(0.0, 1.0, -1.0, 0.0, unrotated_h, 0.0),
        180 => Matrix::new(-1.0, 0.0, 0.0, -1.0, unrotated_w, unrotated_h),
        270 => Matrix::new(0.0, -1.0, 1.0, 0.0, 0.0, unrotated_w),
        _ => Matrix::IDENTITY,
    }
}

fn render_to_pixmap(
    display: &DisplayList,
    width: u32,
    height: u32,
    device: &Matrix,
    scale: f64,
) -> Option<Pixmap> {
    let mut pixmap = Pixmap::new(width, height)?;
    pixmap.fill(SkiaColor::WHITE);

    // Les clips imbriqués sont partagés (Rc) entre de nombreux items
    // consécutifs (tous les glyphes d'une ligne de texte, typiquement) ;
    // on évite de rastériser un nouveau masque à chaque item en réutilisant
    // celui du dernier `ClipStack` vu (comparé par pointeur).
    let mut mask_cache: Option<(*const Vec<pdf_core::display::ClipPath>, Option<Mask>)> = None;
    let mut mask_for = |clip: &Option<ClipStack>| -> Option<Mask> {
        let clip = clip.as_ref()?;
        let ptr = Rc::as_ptr(clip);
        if let Some((cached_ptr, cached_mask)) = &mask_cache {
            if *cached_ptr == ptr {
                return cached_mask.clone();
            }
        }
        let mask = build_clip_mask(clip, width, height, device);
        mask_cache = Some((ptr, mask.clone()));
        mask
    };

    for item in &display.items {
        if let DisplayItem::Path {
            segments,
            paint,
            fill_rule,
            fill_color,
            stroke_color,
            line_width,
            clip,
            ..
        } = item
        {
            let Some(path) = build_path(segments, device) else {
                continue;
            };
            let mask = mask_for(clip);

            if matches!(paint, PaintOp::Fill | PaintOp::FillStroke) {
                let mut paint_fill = Paint::default();
                paint_fill.set_color(to_skia_color(*fill_color));
                paint_fill.anti_alias = true;
                let rule = match fill_rule {
                    FillRule::NonZero => SkiaFillRule::Winding,
                    FillRule::EvenOdd => SkiaFillRule::EvenOdd,
                };
                pixmap.fill_path(
                    &path,
                    &paint_fill,
                    rule,
                    Transform::identity(),
                    mask.as_ref(),
                );
            }

            if matches!(paint, PaintOp::Stroke | PaintOp::FillStroke) {
                let mut paint_stroke = Paint::default();
                paint_stroke.set_color(to_skia_color(*stroke_color));
                paint_stroke.anti_alias = true;
                let stroke = Stroke {
                    width: ((*line_width).max(0.1) * scale) as f32,
                    ..Default::default()
                };
                pixmap.stroke_path(
                    &path,
                    &paint_stroke,
                    &stroke,
                    Transform::identity(),
                    mask.as_ref(),
                );
            }
        } else if let DisplayItem::Glyph {
            outline: Some(segments),
            transform,
            color,
            clip,
            ..
        } = item
        {
            let Some(path) = build_glyph_path(segments, transform, device) else {
                continue;
            };
            let mask = mask_for(clip);
            let mut paint_fill = Paint::default();
            paint_fill.set_color(to_skia_color(*color));
            paint_fill.anti_alias = true;
            pixmap.fill_path(
                &path,
                &paint_fill,
                SkiaFillRule::Winding,
                Transform::identity(),
                mask.as_ref(),
            );
        } else if let DisplayItem::Image {
            pixels: Some(image),
            transform,
            clip,
            ..
        } = item
        {
            let mask = mask_for(clip);
            draw_image(&mut pixmap, image, transform, device, mask.as_ref());
        }
        // DisplayItem::Glyph sans contour et DisplayItem::Image sans pixels
        // décodés : non rendus, voir limitations en tête de module.
    }

    Some(pixmap)
}

/// Rastérise l'intersection d'une chaîne de clips imbriqués (`W`/`W*`) en un
/// masque `tiny_skia` : le premier chemin initialise le masque, les suivants
/// l'intersectent (`Mask::intersect_path`). `None` si aucun chemin de la
/// chaîne ne se rastérise (chemin vide) — dégrade alors en "pas de clip"
/// plutôt que de bloquer tout le rendu.
fn build_clip_mask(
    clip: &[pdf_core::display::ClipPath],
    width: u32,
    height: u32,
    device: &Matrix,
) -> Option<Mask> {
    let mut mask: Option<Mask> = None;
    for clip_path in clip {
        let Some(path) = build_path(&clip_path.segments, device) else {
            continue;
        };
        let rule = match clip_path.fill_rule {
            FillRule::NonZero => SkiaFillRule::Winding,
            FillRule::EvenOdd => SkiaFillRule::EvenOdd,
        };
        match &mut mask {
            None => {
                let mut m = Mask::new(width, height)?;
                m.fill_path(&path, rule, true, Transform::identity());
                mask = Some(m);
            }
            Some(m) => m.intersect_path(&path, rule, true, Transform::identity()),
        }
    }
    mask
}

/// Dessine une image déjà décodée en RGBA8. `transform` positionne le carré
/// unité `[0,1]×[0,1]` (espace image PDF, ISO 32000-1 §8.9.5) dans l'espace
/// de la page ; on compose avec la mise à l'échelle pixel->unité (et le flip
/// vertical propre aux données image, dont la première ligne correspond au
/// *haut* de l'image) puis `device` (flip+rotation page->pixmap communs au
/// reste du pipeline).
fn draw_image(
    pixmap: &mut Pixmap,
    image: &DecodedImage,
    transform: &Matrix,
    device: &Matrix,
    mask: Option<&Mask>,
) {
    if image.width == 0 || image.height == 0 {
        return;
    }
    // `DecodedImage::rgba` est en alpha "straight" (non prémultiplié) ;
    // tiny-skia s'attend à du prémultiplié. Sans `/SMask` (cas courant),
    // alpha vaut 255 partout et les deux représentations coïncident — on
    // évite alors la copie.
    let premultiplied = premultiply_if_needed(&image.rgba);
    let Some(src) = PixmapRef::from_bytes(&premultiplied, image.width, image.height) else {
        return;
    };

    let pixel_to_unit_square = Matrix::new(
        1.0 / image.width as f64,
        0.0,
        0.0,
        -1.0 / image.height as f64,
        0.0,
        1.0,
    );
    let total = pixel_to_unit_square.then(transform).then(device);

    let skia_transform = Transform::from_row(
        total.a as f32,
        total.b as f32,
        total.c as f32,
        total.d as f32,
        total.e as f32,
        total.f as f32,
    );

    pixmap.draw_pixmap(0, 0, src, &PixmapPaint::default(), skia_transform, mask);
}

/// Convertit du RGBA8 "straight" en prémultiplié (format attendu par
/// `tiny_skia::Pixmap`/`PixmapRef`), sans copie si l'image est déjà opaque.
fn premultiply_if_needed(rgba: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    if rgba.chunks_exact(4).all(|p| p[3] == 255) {
        return std::borrow::Cow::Borrowed(rgba);
    }
    let mut out = rgba.to_vec();
    for pixel in out.chunks_exact_mut(4) {
        let a = pixel[3] as u32;
        pixel[0] = ((pixel[0] as u32 * a) / 255) as u8;
        pixel[1] = ((pixel[1] as u32 * a) / 255) as u8;
        pixel[2] = ((pixel[2] as u32 * a) / 255) as u8;
    }
    std::borrow::Cow::Owned(out)
}

/// Matrice PDF (page, origine bas-gauche) -> espace "device" non pivoté
/// (origine haut-gauche), avec mise à l'échelle du zoom incluse. Composée
/// ensuite avec `rotation_matrix` pour obtenir la matrice `device` complète
/// utilisée par le reste du module.
fn page_flip_matrix(origin_x: f64, origin_y_top: f64, scale: f64) -> Matrix {
    Matrix::new(
        scale,
        0.0,
        0.0,
        -scale,
        -origin_x * scale,
        origin_y_top * scale,
    )
}

fn build_path(segments: &[PathSegment], device: &Matrix) -> Option<Path> {
    let map = |p: (f64, f64)| {
        let (x, y) = device.apply(p.0, p.1);
        (x as f32, y as f32)
    };

    let mut pb = PathBuilder::new();
    let mut has_segment = false;
    for seg in segments {
        match seg {
            PathSegment::MoveTo(p) => {
                let (x, y) = map(*p);
                pb.move_to(x, y);
                has_segment = true;
            }
            PathSegment::LineTo(p) => {
                let (x, y) = map(*p);
                pb.line_to(x, y);
                has_segment = true;
            }
            PathSegment::CurveTo { c1, c2, to } => {
                let (x1, y1) = map(*c1);
                let (x2, y2) = map(*c2);
                let (x3, y3) = map(*to);
                pb.cubic_to(x1, y1, x2, y2, x3, y3);
                has_segment = true;
            }
            PathSegment::ClosePath => pb.close(),
        }
    }
    if !has_segment {
        return None;
    }
    pb.finish()
}

/// Comme `build_path`, mais applique d'abord `transform` (matrice de rendu
/// du glyphe : échelle police + matrice texte + CTM) à des points en espace
/// em, avant `device` (flip+rotation page->pixmap).
fn build_glyph_path(segments: &[PathSegment], transform: &Matrix, device: &Matrix) -> Option<Path> {
    let combined = transform.then(device);
    let map = |p: (f64, f64)| {
        let (x, y) = combined.apply(p.0, p.1);
        (x as f32, y as f32)
    };

    let mut pb = PathBuilder::new();
    let mut has_segment = false;
    for seg in segments {
        match seg {
            PathSegment::MoveTo(p) => {
                let (x, y) = map(*p);
                pb.move_to(x, y);
                has_segment = true;
            }
            PathSegment::LineTo(p) => {
                let (x, y) = map(*p);
                pb.line_to(x, y);
                has_segment = true;
            }
            PathSegment::CurveTo { c1, c2, to } => {
                let (x1, y1) = map(*c1);
                let (x2, y2) = map(*c2);
                let (x3, y3) = map(*to);
                pb.cubic_to(x1, y1, x2, y2, x3, y3);
                has_segment = true;
            }
            PathSegment::ClosePath => pb.close(),
        }
    }
    if !has_segment {
        return None;
    }
    pb.finish()
}

fn to_skia_color(color: Color) -> SkiaColor {
    let (r, g, b) = match color {
        Color::Gray(g) => (g, g, g),
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Cmyk(c, m, y, k) => (
            (1.0 - c) * (1.0 - k),
            (1.0 - m) * (1.0 - k),
            (1.0 - y) * (1.0 - k),
        ),
    };
    SkiaColor::from_rgba(
        r.clamp(0.0, 1.0) as f32,
        g.clamp(0.0, 1.0) as f32,
        b.clamp(0.0, 1.0) as f32,
        1.0,
    )
    .unwrap_or(SkiaColor::BLACK)
}

/// Encode un pixmap en PNG (utilitaire de confort pour la CLI/les tests).
pub fn encode_png(pixmap: &Pixmap) -> Vec<u8> {
    pixmap.encode_png().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdf_core::display::DisplayItem;

    fn rect_display(fill: Color) -> DisplayList {
        DisplayList {
            items: vec![DisplayItem::Path {
                segments: vec![
                    PathSegment::MoveTo((10.0, 10.0)),
                    PathSegment::LineTo((90.0, 10.0)),
                    PathSegment::LineTo((90.0, 90.0)),
                    PathSegment::LineTo((10.0, 90.0)),
                    PathSegment::ClosePath,
                ],
                paint: PaintOp::Fill,
                fill_rule: FillRule::NonZero,
                fill_color: fill,
                stroke_color: Color::default(),
                line_width: 1.0,
                sets_clip: false,
                clip: None,
            }],
        }
    }

    #[test]
    fn renders_filled_rect_with_correct_color_and_flip() {
        let display = rect_display(Color::Rgb(1.0, 0.0, 0.0));
        let pixmap = render_page(&display, [0.0, 0.0, 100.0, 100.0]).unwrap();
        assert_eq!(pixmap.width(), 100);
        assert_eq!(pixmap.height(), 100);

        // Le rectangle PDF va de y=10 à y=90 (origine bas-gauche) ; après
        // inversion, son centre doit être peint autour de (50, 50) en
        // coordonnées pixmap (symétrique ici car la page est carrée).
        let center = pixmap.pixel(50, 50).unwrap();
        assert_eq!((center.red(), center.green(), center.blue()), (255, 0, 0));

        // Un point hors du rectangle doit rester blanc.
        let outside = pixmap.pixel(5, 5).unwrap();
        assert_eq!(
            (outside.red(), outside.green(), outside.blue()),
            (255, 255, 255)
        );
    }

    #[test]
    fn scaled_render_produces_larger_pixmap_with_same_content() {
        let display = rect_display(Color::Rgb(1.0, 0.0, 0.0));
        let pixmap = render_page_scaled(&display, [0.0, 0.0, 100.0, 100.0], 2.0).unwrap();
        assert_eq!(pixmap.width(), 200);
        assert_eq!(pixmap.height(), 200);
        // Le centre du rectangle (device (50,50) à l'échelle 1) doit être à
        // (100,100) à l'échelle 2.
        let center = pixmap.pixel(100, 100).unwrap();
        assert_eq!((center.red(), center.green(), center.blue()), (255, 0, 0));
    }

    #[test]
    fn empty_display_list_produces_white_page() {
        let display = DisplayList::default();
        let pixmap = render_page(&display, [0.0, 0.0, 20.0, 20.0]).unwrap();
        let px = pixmap.pixel(10, 10).unwrap();
        assert_eq!((px.red(), px.green(), px.blue()), (255, 255, 255));
    }

    #[test]
    fn png_encoding_produces_nonempty_bytes() {
        let display = rect_display(Color::Gray(0.0));
        let pixmap = render_page(&display, [0.0, 0.0, 50.0, 50.0]).unwrap();
        let png = encode_png(&pixmap);
        assert!(!png.is_empty());
        assert_eq!(
            &png[0..8],
            &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1A, b'\n']
        );
    }

    /// Vérifie l'orientation et le positionnement d'une image décodée : la
    /// moitié gauche est rouge, la droite bleue ; l'image occupe le carré
    /// unité `[0,1]²` mis à l'échelle en `[0,100]²` (page 100×100), donc le
    /// pixel (25,50) de la page doit être rouge et (75,50) bleu — et
    /// l'inversion Y (données image vs. page PDF) ne doit pas les échanger.
    #[test]
    fn draws_decoded_image_at_correct_position() {
        let width = 10u32;
        let height = 10u32;
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for _y in 0..height {
            for x in 0..width {
                if x < width / 2 {
                    rgba.extend_from_slice(&[255, 0, 0, 255]); // rouge à gauche
                } else {
                    rgba.extend_from_slice(&[0, 0, 255, 255]); // bleu à droite
                }
            }
        }
        let image = DecodedImage {
            width,
            height,
            rgba,
        };

        let display = DisplayList {
            items: vec![DisplayItem::Image {
                resource: "Im0".into(),
                transform: Matrix::new(100.0, 0.0, 0.0, 100.0, 0.0, 0.0),
                pixels: Some(image),
                clip: None,
            }],
        };
        let pixmap = render_page(&display, [0.0, 0.0, 100.0, 100.0]).unwrap();

        let left = pixmap.pixel(25, 50).unwrap();
        assert_eq!((left.red(), left.green(), left.blue()), (255, 0, 0));
        let right = pixmap.pixel(75, 50).unwrap();
        assert_eq!((right.red(), right.green(), right.blue()), (0, 0, 255));
    }

    /// Un rectangle noir couvrant toute la page, mais peint sous un clip
    /// 10..50 (`W`/`W*`) : seule cette zone doit être noire, le reste doit
    /// rester blanc — preuve que le masque de clip est réellement appliqué,
    /// pas juste signalé.
    #[test]
    fn clip_restricts_painted_area_to_the_clip_path() {
        use pdf_core::display::ClipPath;

        let clip_rect = vec![
            PathSegment::MoveTo((10.0, 10.0)),
            PathSegment::LineTo((50.0, 10.0)),
            PathSegment::LineTo((50.0, 50.0)),
            PathSegment::LineTo((10.0, 50.0)),
            PathSegment::ClosePath,
        ];
        let clip = Some(Rc::new(vec![ClipPath {
            segments: clip_rect,
            fill_rule: FillRule::NonZero,
        }]));

        let display = DisplayList {
            items: vec![DisplayItem::Path {
                segments: vec![
                    PathSegment::MoveTo((0.0, 0.0)),
                    PathSegment::LineTo((100.0, 0.0)),
                    PathSegment::LineTo((100.0, 100.0)),
                    PathSegment::LineTo((0.0, 100.0)),
                    PathSegment::ClosePath,
                ],
                paint: PaintOp::Fill,
                fill_rule: FillRule::NonZero,
                fill_color: Color::Gray(0.0),
                stroke_color: Color::default(),
                line_width: 1.0,
                sets_clip: false,
                clip,
            }],
        };
        let pixmap = render_page(&display, [0.0, 0.0, 100.0, 100.0]).unwrap();

        // Centre de la zone de clip (10..50, 10..50) -> doit être peint noir.
        let inside = pixmap.pixel(30, 100 - 30).unwrap();
        assert_eq!((inside.red(), inside.green(), inside.blue()), (0, 0, 0));

        // Hors du clip -> doit être resté blanc malgré le rectangle plein page.
        let outside = pixmap.pixel(80, 100 - 80).unwrap();
        assert_eq!(
            (outside.red(), outside.green(), outside.blue()),
            (255, 255, 255)
        );
    }

    #[test]
    fn premultiply_if_needed_skips_copy_when_opaque() {
        let rgba = vec![10, 20, 30, 255, 40, 50, 60, 255];
        let out = premultiply_if_needed(&rgba);
        assert!(matches!(out, std::borrow::Cow::Borrowed(_)));
        assert_eq!(&*out, rgba.as_slice());
    }

    #[test]
    fn premultiply_if_needed_scales_color_by_alpha() {
        // Rouge à 50% d'alpha : (255,0,0,128) -> (~128,0,0,128) prémultiplié.
        let rgba = vec![255u8, 0, 0, 128];
        let out = premultiply_if_needed(&rgba);
        assert!(matches!(out, std::borrow::Cow::Owned(_)));
        assert_eq!(out[0], (255u32 * 128 / 255) as u8);
        assert_eq!(out[1], 0);
        assert_eq!(out[2], 0);
        assert_eq!(out[3], 128);
    }

    /// Une image semi-transparente peinte sur le fond blanc de la page doit
    /// se fondre avec lui (rouge à 50% -> rose), pas juste apparaître en
    /// rouge plein comme si l'alpha était ignoré.
    #[test]
    fn semi_transparent_image_blends_with_page_background() {
        let width = 4u32;
        let height = 4u32;
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..(width * height) {
            rgba.extend_from_slice(&[255, 0, 0, 128]); // rouge à ~50% d'alpha.
        }
        let image = DecodedImage {
            width,
            height,
            rgba,
        };
        let display = DisplayList {
            items: vec![DisplayItem::Image {
                resource: "Im0".into(),
                transform: Matrix::new(100.0, 0.0, 0.0, 100.0, 0.0, 0.0),
                pixels: Some(image),
                clip: None,
            }],
        };
        let pixmap = render_page(&display, [0.0, 0.0, 100.0, 100.0]).unwrap();
        let center = pixmap.pixel(50, 50).unwrap();
        // Ni blanc pur (alpha ignoré à 0) ni rouge pur (alpha ignoré à 255) :
        // un mélange, avec un vert/bleu résiduel du fond blanc.
        assert!(center.green() > 0 && center.green() < 255);
        assert_eq!(center.green(), center.blue());
        assert!(center.red() > center.green());
    }

    /// Bout en bout sur un vrai PDF avec police TrueType intégrée (Monaco) :
    /// parsing -> arbre de pages -> interprétation -> résolution de contour
    /// -> rasterisation. Vérifie qu'au moins un pixel non blanc est peint là
    /// où le texte "AVIL" est positionné (pas de comparaison pixel-perfect,
    /// juste une preuve que le glyphe est réellement dessiné).
    #[test]
    fn renders_real_embedded_font_glyphs() {
        use pdf_core::interp::Interpreter;
        use pdf_core::Document;

        let bytes =
            include_bytes!("../../pdf-core/tests/fixtures/embedded_truetype_font.pdf").to_vec();
        let doc = Document::open(bytes).unwrap();
        let page = doc.page(0).unwrap();
        let content = doc.page_content(&page).unwrap();
        let display = Interpreter::run_page(&doc, page.resources.clone(), &content).unwrap();

        assert!(display.items.iter().any(|item| matches!(
            item,
            DisplayItem::Glyph {
                outline: Some(_),
                ..
            }
        )));

        let pixmap = render_page(&display, page.media_box).unwrap();
        let has_non_white_pixel = (0..pixmap.width()).any(|x| {
            (0..pixmap.height()).any(|y| {
                let px = pixmap.pixel(x, y).unwrap();
                (px.red(), px.green(), px.blue()) != (255, 255, 255)
            })
        });
        assert!(
            has_non_white_pixel,
            "expected glyph ink somewhere on the page"
        );
    }

    /// Comme `renders_real_embedded_font_glyphs`, mais pour une police
    /// CFF/Type1C intégrée (`/FontFile3`) plutôt que TrueType.
    #[test]
    fn renders_real_embedded_cff_font_glyphs() {
        use pdf_core::interp::Interpreter;
        use pdf_core::Document;

        let bytes = include_bytes!("../../pdf-core/tests/fixtures/embedded_cff_font.pdf").to_vec();
        let doc = Document::open(bytes).unwrap();
        let page = doc.page(0).unwrap();
        let content = doc.page_content(&page).unwrap();
        let display = Interpreter::run_page(&doc, page.resources.clone(), &content).unwrap();

        assert!(display.items.iter().any(|item| matches!(
            item,
            DisplayItem::Glyph {
                outline: Some(_),
                ..
            }
        )));

        let pixmap = render_page(&display, page.media_box).unwrap();
        let has_non_white_pixel = (0..pixmap.width()).any(|x| {
            (0..pixmap.height()).any(|y| {
                let px = pixmap.pixel(x, y).unwrap();
                (px.red(), px.green(), px.blue()) != (255, 255, 255)
            })
        });
        assert!(
            has_non_white_pixel,
            "expected CFF glyph ink somewhere on the page"
        );
    }

    /// Bout en bout sur un vrai PDF avec une image semi-transparente
    /// (`/SMask`) recouvrant partiellement un rectangle bleu opaque : la
    /// zone de recouvrement doit être un mélange (violet), ni le bleu pur
    /// du dessous ni le rouge cramoisi pur de l'image (ce qui indiquerait
    /// que l'alpha a été ignoré dans un sens ou dans l'autre).
    #[test]
    fn renders_real_smask_image_blended_over_opaque_rect() {
        use pdf_core::interp::Interpreter;
        use pdf_core::Document;

        let bytes = include_bytes!("../../pdf-core/tests/fixtures/image_smask.pdf").to_vec();
        let doc = Document::open(bytes).unwrap();
        let page = doc.page(0).unwrap();
        let content = doc.page_content(&page).unwrap();
        let display = Interpreter::run_page(&doc, page.resources.clone(), &content).unwrap();
        let pixmap = render_page(&display, page.media_box).unwrap();

        // La page fait 612x792 ; le rectangle bleu va de (50,600) à
        // (250,750) en espace PDF, l'image de (100,620) à (250,770) -- leur
        // recouvrement est autour de (150-250, 620-750). On sonde un point
        // qui devrait être dans cette zone après inversion d'axe Y.
        let px = pixmap.pixel(180, 792 - 680).unwrap();
        assert!(px.blue() > 0, "expected residual blue from the rect below");
        assert!(
            px.red() > 0,
            "expected residual red from the translucent image"
        );
        assert!(
            !(px.red() > 240 && px.green() < 15 && px.blue() < 15),
            "pixel looks fully opaque crimson: alpha was likely ignored"
        );
        assert_ne!(
            (px.red(), px.green(), px.blue()),
            (0, 0, 255),
            "pixel looks like pure opaque blue: image was likely not drawn"
        );
    }

    /// `render_page` (sans rotation) et `render_page_rotated(rotate=0)`
    /// doivent produire des dimensions identiques : la normalisation ne
    /// doit rien changer pour la valeur la plus courante.
    #[test]
    fn rotate_zero_matches_unrotated_dimensions() {
        let display = rect_display(Color::Rgb(1.0, 0.0, 0.0));
        let plain = render_page(&display, [0.0, 0.0, 100.0, 60.0]).unwrap();
        let rotated = render_page_rotated(&display, [0.0, 0.0, 100.0, 60.0], 0, 1.0).unwrap();
        assert_eq!((plain.width(), plain.height()), (100, 60));
        assert_eq!((rotated.width(), rotated.height()), (100, 60));
    }

    /// `/Rotate 90` doit permuter largeur/hauteur du pixmap (portrait ->
    /// paysage) — c'est le signe le plus visible que la rotation est
    /// appliquée plutôt qu'ignorée.
    #[test]
    fn rotate_90_swaps_pixmap_dimensions() {
        let display = rect_display(Color::Rgb(1.0, 0.0, 0.0));
        let pixmap = render_page_rotated(&display, [0.0, 0.0, 100.0, 60.0], 90, 1.0).unwrap();
        assert_eq!((pixmap.width(), pixmap.height()), (60, 100));
    }

    /// Un rectangle qui occupe le coin bas-gauche de la page (espace PDF)
    /// doit se retrouver dans le coin haut-gauche du canevas final après une
    /// rotation `/Rotate 90` (rotation horaire de la page vue du lecteur —
    /// voir la dérivation dans `rotation_matrix`).
    #[test]
    fn rotate_90_moves_bottom_left_content_to_top_left() {
        let display = DisplayList {
            items: vec![DisplayItem::Path {
                segments: vec![
                    PathSegment::MoveTo((0.0, 0.0)),
                    PathSegment::LineTo((20.0, 0.0)),
                    PathSegment::LineTo((20.0, 20.0)),
                    PathSegment::LineTo((0.0, 20.0)),
                    PathSegment::ClosePath,
                ],
                paint: PaintOp::Fill,
                fill_rule: FillRule::NonZero,
                fill_color: Color::Gray(0.0),
                stroke_color: Color::default(),
                line_width: 1.0,
                sets_clip: false,
                clip: None,
            }],
        };
        let pixmap = render_page_rotated(&display, [0.0, 0.0, 100.0, 60.0], 90, 1.0).unwrap();
        assert_eq!((pixmap.width(), pixmap.height()), (60, 100));

        let top_left = pixmap.pixel(5, 5).unwrap();
        assert_eq!(
            (top_left.red(), top_left.green(), top_left.blue()),
            (0, 0, 0),
            "bottom-left PDF content should land top-left after a 90° rotation"
        );
        let bottom_right = pixmap.pixel(55, 95).unwrap();
        assert_eq!(
            (
                bottom_right.red(),
                bottom_right.green(),
                bottom_right.blue()
            ),
            (255, 255, 255)
        );
    }
}
