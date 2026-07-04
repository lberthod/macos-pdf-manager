//! Types de la « liste d'affichage » (display list) — architecture.md §4.5 :
//! sortie de l'interpréteur de flux de contenu, indépendante du langage
//! d'opérateurs PDF. Consommée par le futur rendu (`pdf-render`) et
//! l'extraction de texte (`pdf-text`).

/// Matrice de transformation affine PDF, convention vecteur-ligne :
/// `[x' y' 1] = [x y 1] * [[a b 0][c d 0][e f 1]]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Matrix {
    pub const IDENTITY: Matrix = Matrix {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    pub fn new(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Self {
        Self { a, b, c, d, e, f }
    }

    pub fn translation(tx: f64, ty: f64) -> Self {
        Self::new(1.0, 0.0, 0.0, 1.0, tx, ty)
    }

    /// Compose `self` puis `other` (un point subit d'abord `self`, puis
    /// `other`) : équivalent au produit matriciel `self * other` en
    /// convention vecteur-ligne.
    pub fn then(&self, other: &Matrix) -> Matrix {
        Matrix {
            a: self.a * other.a + self.b * other.c,
            b: self.a * other.b + self.b * other.d,
            c: self.c * other.a + self.d * other.c,
            d: self.c * other.b + self.d * other.d,
            e: self.e * other.a + self.f * other.c + other.e,
            f: self.e * other.b + self.f * other.d + other.f,
        }
    }

    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Color {
    Gray(f64),
    Rgb(f64, f64, f64),
    Cmyk(f64, f64, f64, f64),
}

impl Default for Color {
    fn default() -> Self {
        Color::Gray(0.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaintOp {
    None,
    Fill,
    Stroke,
    FillStroke,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathSegment {
    MoveTo((f64, f64)),
    LineTo((f64, f64)),
    CurveTo {
        c1: (f64, f64),
        c2: (f64, f64),
        to: (f64, f64),
    },
    ClosePath,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DisplayItem {
    Path {
        segments: Vec<PathSegment>,
        paint: PaintOp,
        fill_rule: FillRule,
        fill_color: Color,
        stroke_color: Color,
        line_width: f64,
        /// Le chemin délimite aussi la zone de clip courante (opérateur
        /// `W`/`W*`) — l'application réelle du clip est laissée au rendu
        /// (Sprint 7+, voir sprint.md).
        sets_clip: bool,
    },
    /// Un glyphe positionné. `code` est le code de caractère brut tel
    /// qu'il apparaît dans la chaîne du flux de contenu. `unicode` est
    /// résolu via `/Encoding` + `/Differences` (Sprint 7-8) quand la police
    /// est une police simple 1 octet reconnue ; `None` sinon (police
    /// composite `/Type0` ou absente des ressources). `advance_is_estimated`
    /// signale que l'avance utilisée pour positionner le glyphe *suivant*
    /// est une heuristique constante plutôt qu'une largeur de police réelle.
    /// `outline` contient le contour vectoriel du glyphe (espace em, déjà
    /// combiné à `transform` pour obtenir l'espace page) quand une police
    /// TrueType intégrée fournit un contour réel (Sprint 7-8+) ; `None` si
    /// aucun contour n'est disponible (police standard non intégrée,
    /// CFF/Type1, ou glyphe absent du `cmap`) — le glyphe n'est alors pas
    /// dessinable tel quel par le rendu (voir `pdf-render`).
    Glyph {
        font: String,
        code: u32,
        unicode: Option<char>,
        transform: Matrix,
        color: Color,
        advance_is_estimated: bool,
        outline: Option<Vec<PathSegment>>,
    },
    /// XObject image. `pixels` est `None` si le décodage a échoué ou si le
    /// format n'est pas supporté (`CCITTFaxDecode`, `JBIG2Decode`,
    /// `JPXDecode`, espaces colorimétriques indexés/Separation, profondeurs
    /// autres que 8 bits/composante — voir `image.rs`).
    Image {
        resource: String,
        transform: Matrix,
        pixels: Option<DecodedImage>,
    },
}

/// Image bitmap déjà décodée en RGBA8, prête à dessiner — voir `image.rs`.
/// L'unité image PDF ([0,1]×[0,1]) correspond à ce bitmap ; c'est `transform`
/// (sur `DisplayItem::Image`) qui la positionne dans l'espace de la page.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` octets, RGBA8 ligne par ligne depuis le haut de
    /// l'image (convention raster standard). Alpha toujours 255 : pas de
    /// support `/SMask` (canal de transparence) pour l'instant.
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DisplayList {
    pub items: Vec<DisplayItem>,
}

impl DisplayList {
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_matrix_is_noop() {
        assert_eq!(Matrix::IDENTITY.apply(3.0, 4.0), (3.0, 4.0));
    }

    #[test]
    fn translation_then_scale_composes_correctly() {
        let translate = Matrix::translation(10.0, 20.0);
        let scale = Matrix::new(2.0, 0.0, 0.0, 2.0, 0.0, 0.0);
        // Point (1,1) -> translate -> (11,21) -> scale -> (22,42).
        let combined = translate.then(&scale);
        assert_eq!(combined.apply(1.0, 1.0), (22.0, 42.0));
    }
}
