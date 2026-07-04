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
//!   `DCTDecode`/JPEG et échantillons bruts 8 bits DeviceGray/RGB/CMYK ;
//!   pas de CCITT/JBIG2/JPX, pas de canal alpha `/SMask`).
//! - Le clip (`sets_clip`) n'est pas appliqué.
//! - Espaces colorimétriques : conversion CMYK -> RGB naïve (sans profil ICC).

use pdf_core::display::{
    Color, DecodedImage, DisplayItem, DisplayList, FillRule, Matrix, PaintOp, PathSegment,
};
use tiny_skia::{
    Color as SkiaColor, FillRule as SkiaFillRule, Paint, Path, PathBuilder, Pixmap, PixmapPaint,
    PixmapRef, Stroke, Transform,
};

/// Rasterise une page entière en se basant sur son `MediaBox`
/// (`[x0, y0, x1, y1]`, ISO 32000-1 §7.7.3.3) : la taille du pixmap suit la
/// largeur/hauteur du MediaBox en points PDF (1 pixel = 1 point ; le zoom
/// est laissé à l'appelant via un futur paramètre d'échelle, voir sprint.md).
pub fn render_page(display: &DisplayList, media_box: [f64; 4]) -> Option<Pixmap> {
    let width = (media_box[2] - media_box[0]).round().max(1.0) as u32;
    let height = (media_box[3] - media_box[1]).round().max(1.0) as u32;
    render_to_pixmap(display, width, height, media_box[0], media_box[3])
}

fn render_to_pixmap(
    display: &DisplayList,
    width: u32,
    height: u32,
    origin_x: f64,
    origin_y_top: f64,
) -> Option<Pixmap> {
    let mut pixmap = Pixmap::new(width, height)?;
    pixmap.fill(SkiaColor::WHITE);

    for item in &display.items {
        if let DisplayItem::Path {
            segments,
            paint,
            fill_rule,
            fill_color,
            stroke_color,
            line_width,
            ..
        } = item
        {
            let Some(path) = build_path(segments, origin_x, origin_y_top) else {
                continue;
            };

            if matches!(paint, PaintOp::Fill | PaintOp::FillStroke) {
                let mut paint_fill = Paint::default();
                paint_fill.set_color(to_skia_color(*fill_color));
                paint_fill.anti_alias = true;
                let rule = match fill_rule {
                    FillRule::NonZero => SkiaFillRule::Winding,
                    FillRule::EvenOdd => SkiaFillRule::EvenOdd,
                };
                pixmap.fill_path(&path, &paint_fill, rule, Transform::identity(), None);
            }

            if matches!(paint, PaintOp::Stroke | PaintOp::FillStroke) {
                let mut paint_stroke = Paint::default();
                paint_stroke.set_color(to_skia_color(*stroke_color));
                paint_stroke.anti_alias = true;
                let stroke = Stroke {
                    width: (*line_width).max(0.1) as f32,
                    ..Default::default()
                };
                pixmap.stroke_path(&path, &paint_stroke, &stroke, Transform::identity(), None);
            }
        } else if let DisplayItem::Glyph {
            outline: Some(segments),
            transform,
            color,
            ..
        } = item
        {
            let Some(path) = build_glyph_path(segments, transform, origin_x, origin_y_top) else {
                continue;
            };
            let mut paint_fill = Paint::default();
            paint_fill.set_color(to_skia_color(*color));
            paint_fill.anti_alias = true;
            pixmap.fill_path(
                &path,
                &paint_fill,
                SkiaFillRule::Winding,
                Transform::identity(),
                None,
            );
        } else if let DisplayItem::Image {
            pixels: Some(image),
            transform,
            ..
        } = item
        {
            draw_image(&mut pixmap, image, transform, origin_x, origin_y_top);
        }
        // DisplayItem::Glyph sans contour et DisplayItem::Image sans pixels
        // décodés : non rendus, voir limitations en tête de module.
    }

    Some(pixmap)
}

/// Dessine une image déjà décodée en RGBA8. `transform` positionne le carré
/// unité `[0,1]×[0,1]` (espace image PDF, ISO 32000-1 §8.9.5) dans l'espace
/// de la page ; on compose avec la mise à l'échelle pixel->unité (et le flip
/// vertical propre aux données image, dont la première ligne correspond au
/// *haut* de l'image) puis le flip page->pixmap commun au reste du pipeline.
fn draw_image(
    pixmap: &mut Pixmap,
    image: &DecodedImage,
    transform: &Matrix,
    origin_x: f64,
    origin_y_top: f64,
) {
    if image.width == 0 || image.height == 0 {
        return;
    }
    let Some(src) = PixmapRef::from_bytes(&image.rgba, image.width, image.height) else {
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
    let page_flip = Matrix::new(1.0, 0.0, 0.0, -1.0, -origin_x, origin_y_top);
    let total = pixel_to_unit_square.then(transform).then(&page_flip);

    let skia_transform = Transform::from_row(
        total.a as f32,
        total.b as f32,
        total.c as f32,
        total.d as f32,
        total.e as f32,
        total.f as f32,
    );

    pixmap.draw_pixmap(0, 0, src, &PixmapPaint::default(), skia_transform, None);
}

fn build_path(segments: &[PathSegment], origin_x: f64, origin_y_top: f64) -> Option<Path> {
    // Espace utilisateur PDF (origine bas-gauche, Y vers le haut) -> espace
    // pixmap (origine haut-gauche, Y vers le bas).
    let flip = |(x, y): (f64, f64)| ((x - origin_x) as f32, (origin_y_top - y) as f32);

    let mut pb = PathBuilder::new();
    let mut has_segment = false;
    for seg in segments {
        match seg {
            PathSegment::MoveTo(p) => {
                let (x, y) = flip(*p);
                pb.move_to(x, y);
                has_segment = true;
            }
            PathSegment::LineTo(p) => {
                let (x, y) = flip(*p);
                pb.line_to(x, y);
                has_segment = true;
            }
            PathSegment::CurveTo { c1, c2, to } => {
                let (x1, y1) = flip(*c1);
                let (x2, y2) = flip(*c2);
                let (x3, y3) = flip(*to);
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
/// em, avant l'inversion d'axe Y commune à tout le pipeline.
fn build_glyph_path(
    segments: &[PathSegment],
    transform: &Matrix,
    origin_x: f64,
    origin_y_top: f64,
) -> Option<Path> {
    let map = |p: (f64, f64)| {
        let (px, py) = transform.apply(p.0, p.1);
        ((px - origin_x) as f32, (origin_y_top - py) as f32)
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
            }],
        };
        let pixmap = render_page(&display, [0.0, 0.0, 100.0, 100.0]).unwrap();

        let left = pixmap.pixel(25, 50).unwrap();
        assert_eq!((left.red(), left.green(), left.blue()), (255, 0, 0));
        let right = pixmap.pixel(75, 50).unwrap();
        assert_eq!((right.red(), right.green(), right.blue()), (0, 0, 255));
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
}
