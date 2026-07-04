//! Backend de rendu GPU (`wgpu` + tessellation `lyon`) — Sprint 9-10,
//! alternative au backend CPU `tiny-skia` de `pdf-render`.
//!
//! Troisième tranche, toujours volontairement limitée par rapport à
//! `pdf-render` :
//! - Chemins **remplis** (`PaintOp::Fill`/`FillStroke`) et **tracés**
//!   (`PaintOp::Stroke`/`FillStroke`), et glyphes (toujours remplis, règle
//!   nonzero), tessellés via `lyon::tessellation::{FillTessellator,
//!   StrokeTessellator}`.
//! - Rotation de page (`/Rotate`, voir `render_page_rotated`) appliquée
//!   directement dans l'espace NDC (voir `PageToNdc::map_to_ndc`) plutôt
//!   qu'en pixels comme côté CPU — la structure mathématique est identique
//!   à `pdf_render::rotation_matrix`, seul l'espace de travail change.
//! - **Images** (`DisplayItem::Image`) dessinées comme un quad texturé
//!   (voir `build_image_quad`), avec le même mapping pixel -> carré unité
//!   -> espace page que `pdf_render::draw_image` (`y` inversé : ligne 0 de
//!   `DecodedImage` = haut de l'image = `y=1` du carré unité).
//! - **Clip** (`W`/`W*`, voir `DisplayItem::*::clip`) appliqué via un
//!   stencil buffer plutôt que le `tiny_skia::Mask` du CPU : les items sont
//!   regroupés par pile de clip (`ClipStack`, comparée par pointeur `Rc`,
//!   voir `group_items`), et chaque groupe rend d'abord ses `ClipPath`
//!   imbriqués dans le stencil (`clip_write_pipeline`, incrémenté couche par
//!   couche — la couche `i` ne s'écrit que là où la couche `i-1` a déjà
//!   marqué le stencil, ce qui accumule l'**intersection** des clips), puis
//!   son contenu avec un test stencil "= profondeur totale". Un stencil de
//!   0 (donc un `Always` pour le groupe non clippé) évite un test inutile.
//! - Glyphes tessellés une seule fois par `(font, code)` **par appel**
//!   (voir `GlyphCache`) : le contour em-space est mis en cache après sa
//!   première tessellation, puis simplement transformé (multiplication de
//!   matrice, pas de nouvelle tessellation `lyon`) pour chaque occurrence
//!   suivante du même glyphe sur la page — un texte de plusieurs centaines
//!   de caractères ne tessellera dans la pratique qu'une poignée de formes
//!   distinctes. Toujours pas un atlas de texture inter-appels (ce cache ne
//!   survit pas à un appel à `render_page`/`GpuRenderer::render_page*`, qui
//!   recrée pipelines/shaders/textures à chaque fois, voir plus bas).
//! - Rendu hors-écran par construction (produit un `RenderedPage` RGBA8 lu
//!   depuis une texture, pas encore un dessin direct dans une surface
//!   `pdf-ui`) — mais le `Device`/`Queue` `wgpu`, eux, peuvent maintenant
//!   être partagés entre appels : `render_page`/`render_page_scaled`/
//!   `render_page_rotated` (fonctions libres) créent toujours un contexte
//!   headless éphémère (`Instance::request_adapter` sans surface) à chaque
//!   appel — pratique pour les tests et un usage ponctuel, mais chaque appel
//!   renégocie un device (poignée de main avec le driver). `GpuRenderer`
//!   évite ce coût : construit une fois (`GpuRenderer::new()`, ou
//!   `from_shared` pour réutiliser le `Device`/`Queue` déjà négociés par un
//!   hôte comme `eframe` — voir `pdf-ui`, qui sélectionne son backend
//!   `wgpu` et lui passe `egui_wgpu::RenderState::{device,queue}`), il rend
//!   ensuite autant de pages qu'on veut sans nouvelle négociation
//!   d'adaptateur, seule la partie réellement coûteuse par page (pipelines/
//!   tessellation) restant répétée.
//!
//! Convention de coordonnées : contrairement à `pdf-render` (espace pixmap,
//! origine haut-gauche, Y vers le bas, d'où le flip explicite), l'espace
//! de clip `wgpu`/NDC a Y vers le haut — la même orientation que l'espace
//! page PDF. Le mapping page -> NDC est donc une simple mise à l'échelle
//! linéaire, sans inversion d'axe (voir `PageToNdc`).

use lyon::math::point;
use lyon::path::Path as LyonPath;
use lyon::tessellation::{
    BuffersBuilder, FillOptions, FillRule as LyonFillRule, FillTessellator, FillVertex,
    FillVertexConstructor, StrokeOptions, StrokeTessellator, StrokeVertex, StrokeVertexConstructor,
    VertexBuffers,
};
use pdf_core::display::{ClipStack, Color, DisplayItem, DisplayList, FillRule, Matrix, PaintOp, PathSegment};
use std::rc::Rc;

/// Image RGBA8 rastérisée par ce backend — même forme que
/// `pdf_app::RenderedPage`/le pixmap de `pdf-render`, pour rester
/// interchangeable côté appelant.
pub struct RenderedPage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Rastérise une page entière (voir les limitations en tête de module) en
/// créant un contexte `wgpu` headless éphémère (voir la doc de module) —
/// pour un rendu interactif répété (`pdf-ui`), préférer `GpuRenderer`, qui
/// réutilise un `Device`/`Queue` déjà existant (typiquement celui
/// d'`eframe`) plutôt que d'en renégocier un nouveau à chaque page.
/// `None` si aucun adaptateur `wgpu` n'est disponible dans l'environnement
/// (pas de GPU/driver) — l'appelant doit alors se rabattre sur `pdf-render`.
pub fn render_page(display: &DisplayList, media_box: [f64; 4]) -> Option<RenderedPage> {
    render_page_scaled(display, media_box, 1.0)
}

/// Comme `render_page`, avec un facteur d'échelle.
pub fn render_page_scaled(
    display: &DisplayList,
    media_box: [f64; 4],
    scale: f64,
) -> Option<RenderedPage> {
    render_page_rotated(display, media_box, 0, scale)
}

/// Comme `render_page_scaled`, en appliquant en plus la rotation de page
/// (`/Rotate`, ISO 32000-1 §7.7.3.3).
pub fn render_page_rotated(
    display: &DisplayList,
    media_box: [f64; 4],
    rotate: i32,
    scale: f64,
) -> Option<RenderedPage> {
    GpuRenderer::new()?.render_page_rotated(display, media_box, rotate, scale)
}

/// Rastérise avec un `wgpu::Device`/`Queue` conservé entre les appels — voir
/// la doc de module : les pipelines/shaders/textures d'une page restent
/// recréés à chaque appel (documenté comme un coût acceptable, largement
/// dominé par la tessellation `lyon`), mais la négociation d'adaptateur/
/// device (`Instance::request_adapter`/`request_device`, une poignée de main
/// avec le driver) ne l'est plus — le coût qui rendait ce backend
/// inutilisable dans une boucle interactive (voir `render_page`).
///
/// Construit via `new()` (contexte headless dédié, comme les fonctions
/// libres ci-dessus) ou `from_shared` (réutilise le `Device`/`Queue` d'un
/// hôte existant, typiquement `eframe`'s `egui_wgpu::RenderState` quand
/// `NativeOptions::renderer` vaut `Wgpu` — voir `pdf-ui`).
#[derive(Clone)]
pub struct GpuRenderer(GpuContext);

impl GpuRenderer {
    /// Comme les fonctions libres du module : négocie son propre
    /// `Device`/`Queue` headless. `None` si aucun adaptateur `wgpu` n'est
    /// disponible.
    pub fn new() -> Option<Self> {
        GpuContext::new().map(GpuRenderer)
    }

    /// Réutilise un `Device`/`Queue` déjà négociés par l'appelant (partagés
    /// via `Arc`, comme les expose `egui_wgpu::RenderState`) plutôt que d'en
    /// renégocier — c'est ce qui rend ce backend viable dans une boucle de
    /// rendu interactive (voir la doc de `GpuRenderer`).
    pub fn from_shared(device: std::sync::Arc<wgpu::Device>, queue: std::sync::Arc<wgpu::Queue>) -> Self {
        GpuRenderer(GpuContext { device, queue })
    }

    pub fn render_page(&self, display: &DisplayList, media_box: [f64; 4]) -> Option<RenderedPage> {
        self.render_page_scaled(display, media_box, 1.0)
    }

    pub fn render_page_scaled(
        &self,
        display: &DisplayList,
        media_box: [f64; 4],
        scale: f64,
    ) -> Option<RenderedPage> {
        self.render_page_rotated(display, media_box, 0, scale)
    }

    /// Comme `render_page_scaled`, en appliquant en plus la rotation de page
    /// (`/Rotate`, ISO 32000-1 §7.7.3.3). Les dimensions du pixmap sont
    /// permutées pour 90°/270° (portrait -> paysage), comme côté
    /// `pdf_render::render_page_rotated`.
    pub fn render_page_rotated(
        &self,
        display: &DisplayList,
        media_box: [f64; 4],
        rotate: i32,
        scale: f64,
    ) -> Option<RenderedPage> {
        let scale = scale.max(0.01);
        let rotate = normalize_rotate(rotate);
        let unrotated_w = ((media_box[2] - media_box[0]) * scale).round().max(1.0) as u32;
        let unrotated_h = ((media_box[3] - media_box[1]) * scale).round().max(1.0) as u32;
        let (width, height) = if rotate == 90 || rotate == 270 {
            (unrotated_h, unrotated_w)
        } else {
            (unrotated_w, unrotated_h)
        };

        self.0.render(display, media_box, rotate, width, height)
    }
}

/// Normalise une valeur `/Rotate` arbitraire au multiple de 90 le plus
/// proche dans `{0, 90, 180, 270}` — même comportement que
/// `pdf_render`'s (non partagée : fonction pure minuscule, pas besoin
/// d'introduire une dépendance entre les deux crates de rendu pour ça).
fn normalize_rotate(rotate: i32) -> i32 {
    let r = rotate.rem_euclid(360);
    match r {
        315..=359 | 0..=44 => 0,
        45..=134 => 90,
        135..=224 => 180,
        _ => 270,
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 2],
    color: [f32; 4],
}

const SHADER: &str = r#"
struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) color: vec4<f32>,
};
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = vec4<f32>(in.position, 0.0, 1.0);
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}
"#;

/// Mappe l'espace page PDF (origine bas-gauche, Y vers le haut, comme
/// `MediaBox`) vers l'espace de clip `wgpu` ([-1,1]², Y vers le haut aussi)
/// — une mise à l'échelle linéaire (sans inversion d'axe, voir la doc de
/// module) suivie d'une rotation dans le carré NDC pour `/Rotate`.
///
/// Dérivation de la rotation (dans un carré [-1,1]² centré, donc pas besoin
/// de connaître les dimensions en points comme `pdf_render::rotation_matrix`,
/// qui travaillait en pixels) : pivoter la page dans le sens horaire déplace
/// son coin haut-gauche (-1,1) vers le coin haut-droit (1,1) du carré pour
/// 90°, ce que donne la rotation `(x,y) -> (y,-x)` — vérifiée par
/// `rotate_90_moves_top_left_content_to_top_right`.
#[derive(Clone, Copy)]
struct PageToNdc {
    origin_x: f64,
    origin_y: f64,
    width_pts: f64,
    height_pts: f64,
    /// Normalisé dans `{0, 90, 180, 270}` (voir `normalize_rotate`).
    rotate: i32,
}

impl PageToNdc {
    fn map_to_ndc(&self, x: f64, y: f64) -> [f32; 2] {
        let nx = ((x - self.origin_x) / self.width_pts) * 2.0 - 1.0;
        let ny = ((y - self.origin_y) / self.height_pts) * 2.0 - 1.0;
        let (rx, ry) = match self.rotate {
            90 => (ny, -nx),
            180 => (-nx, -ny),
            270 => (-ny, nx),
            _ => (nx, ny),
        };
        [rx as f32, ry as f32]
    }
}

struct WithColor {
    color: [f32; 4],
    mapper: PageToNdc,
}

impl FillVertexConstructor<Vertex> for WithColor {
    fn new_vertex(&mut self, vertex: FillVertex) -> Vertex {
        let p = vertex.position();
        Vertex {
            position: self.mapper.map_to_ndc(p.x as f64, p.y as f64),
            color: self.color,
        }
    }
}

impl StrokeVertexConstructor<Vertex> for WithColor {
    fn new_vertex(&mut self, vertex: StrokeVertex) -> Vertex {
        let p = vertex.position();
        Vertex {
            position: self.mapper.map_to_ndc(p.x as f64, p.y as f64),
            color: self.color,
        }
    }
}

/// Constructeur de sommets "brut" : ne fait aucune mise à l'échelle ni
/// mapping, juste la position em-space telle que tessellée par `lyon` —
/// utilisé pour peupler `GlyphCache` (voir plus bas), où le mapping page ->
/// NDC et la couleur ne sont appliqués qu'au moment de consommer le cache,
/// une fois par occurrence du glyphe plutôt qu'une fois par tessellation.
struct RawPosition;

impl FillVertexConstructor<[f32; 2]> for RawPosition {
    fn new_vertex(&mut self, vertex: FillVertex) -> [f32; 2] {
        let p = vertex.position();
        [p.x, p.y]
    }
}

/// Géométrie tessellée d'un glyphe en espace em (non transformée) : voir
/// `RawPosition`.
type CachedGlyphGeometry = (Vec<[f32; 2]>, Vec<u32>);

/// Cache `(font, code) -> géométrie em-space déjà tessellée` valide pour la
/// durée d'un seul appel à `group_items` (voir la doc de module — pas un
/// atlas persistant entre appels). `None` mémorise un glyphe dont le
/// contour ne produit aucune géométrie exploitable (chemin dégénéré), pour
/// éviter de retenter sa tessellation à chaque occurrence.
type GlyphCache = std::collections::HashMap<(String, u32), Option<CachedGlyphGeometry>>;

/// Transforme une géométrie de glyphe mise en cache (em-space) par
/// `transform` (échelle police + matrice texte + CTM, voir `pdf_core::interp`)
/// puis par `mapper` (page -> NDC), et l'ajoute à `out` — évite de
/// re-tesseller ce qui a déjà été tessellé pour une occurrence précédente du
/// même glyphe (voir `GlyphCache`).
fn append_cached_glyph(
    cached: &CachedGlyphGeometry,
    transform: &Matrix,
    color: [f32; 4],
    mapper: &PageToNdc,
    out: &mut VertexBuffers<Vertex, u32>,
) {
    let (positions, indices) = cached;
    let base = out.vertices.len() as u32;
    out.vertices.extend(positions.iter().map(|p| {
        let (px, py) = transform.apply(p[0] as f64, p[1] as f64);
        Vertex {
            position: mapper.map_to_ndc(px, py),
            color,
        }
    }));
    out.indices.extend(indices.iter().map(|i| i + base));
}

fn to_rgba(color: Color) -> [f32; 4] {
    let (r, g, b) = match color {
        Color::Gray(g) => (g, g, g),
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Cmyk(c, m, y, k) => (
            (1.0 - c) * (1.0 - k),
            (1.0 - m) * (1.0 - k),
            (1.0 - y) * (1.0 - k),
        ),
    };
    [
        r.clamp(0.0, 1.0) as f32,
        g.clamp(0.0, 1.0) as f32,
        b.clamp(0.0, 1.0) as f32,
        1.0,
    ]
}

fn to_lyon_fill_rule(rule: FillRule) -> LyonFillRule {
    match rule {
        FillRule::NonZero => LyonFillRule::NonZero,
        FillRule::EvenOdd => LyonFillRule::EvenOdd,
    }
}

/// Construit un chemin `lyon` à partir de segments déjà en espace page
/// (coordonnées absolues) — pas de conversion d'espace ici, seulement de
/// représentation (`PathSegment` -> primitives `lyon::path::Builder`).
fn build_lyon_path(segments: &[PathSegment]) -> Option<LyonPath> {
    let mut builder = LyonPath::builder();
    let mut has_segment = false;
    let mut open = false;

    for seg in segments {
        match seg {
            PathSegment::MoveTo((x, y)) => {
                if open {
                    builder.end(false);
                }
                builder.begin(point(*x as f32, *y as f32));
                open = true;
                has_segment = true;
            }
            PathSegment::LineTo((x, y)) => {
                if !open {
                    builder.begin(point(*x as f32, *y as f32));
                    open = true;
                }
                builder.line_to(point(*x as f32, *y as f32));
                has_segment = true;
            }
            PathSegment::CurveTo { c1, c2, to } => {
                if !open {
                    builder.begin(point(c1.0 as f32, c1.1 as f32));
                    open = true;
                }
                builder.cubic_bezier_to(
                    point(c1.0 as f32, c1.1 as f32),
                    point(c2.0 as f32, c2.1 as f32),
                    point(to.0 as f32, to.1 as f32),
                );
                has_segment = true;
            }
            PathSegment::ClosePath => {
                if open {
                    builder.close();
                    open = false;
                }
            }
        }
    }
    if open {
        builder.end(false);
    }
    if !has_segment {
        return None;
    }
    Some(builder.build())
}

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct TexVertex {
    position: [f32; 2],
    uv: [f32; 2],
}

const IMAGE_SHADER: &str = r#"
struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
};
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = vec4<f32>(in.position, 0.0, 1.0);
    out.uv = in.uv;
    return out;
}

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(tex, samp, in.uv);
}
"#;

/// Construit un quad texturé plaçant `image` dans l'espace page via
/// `transform` (carré unité PDF -> espace page, voir la doc de
/// `DisplayItem::Image`), puis en NDC via `mapper` — même mapping pixel ->
/// carré unité que `pdf_render::draw_image` : la ligne 0 de `DecodedImage`
/// (haut de l'image) correspond à `y=1` du carré unité (`uv.y = 1 - y`,
/// puisque la ligne 0 d'une texture `wgpu` est aussi son `v=0`).
fn build_image_quad(transform: &Matrix, mapper: &PageToNdc) -> ([TexVertex; 4], [u16; 6]) {
    let corners = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
    let mut vertices = [TexVertex {
        position: [0.0, 0.0],
        uv: [0.0, 0.0],
    }; 4];
    for (i, (ux, uy)) in corners.iter().enumerate() {
        let (px, py) = transform.apply(*ux, *uy);
        vertices[i] = TexVertex {
            position: mapper.map_to_ndc(px, py),
            uv: [*ux as f32, (1.0 - *uy) as f32],
        };
    }
    (vertices, [0, 1, 2, 0, 2, 3])
}

/// Un item de contenu à peindre au sein d'un `Group` (voir plus bas),
/// préservant l'ordre de peinture d'origine de la `DisplayList` — chemins
/// et glyphes d'un côté (accumulés dans un seul buffer de géométrie tant
/// qu'aucune image ne s'intercale, pour limiter le nombre d'appels de
/// dessin), images texturées de l'autre (chacune son propre bind group).
enum Draw {
    Solid(VertexBuffers<Vertex, u32>),
    Image {
        width: u32,
        height: u32,
        rgba: Vec<u8>,
        quad: ([TexVertex; 4], [u16; 6]),
    },
}

/// Un groupe d'items partageant la même pile de clip (comparée par
/// pointeur `Rc`, voir `group_items`) — la frontière entre deux groupes est
/// aussi une frontière de passe de rendu (voir `GpuContext::render`), pour
/// pouvoir remettre à zéro le stencil buffer entre deux piles de clip
/// différentes sans perdre le contenu déjà peint (`LoadOp::Load` sur la
/// couleur, `LoadOp::Clear` sur le stencil).
struct Group {
    /// Une couche de géométrie tessellée par `ClipPath` de la pile (déjà en
    /// NDC, via le même `mapper` que le reste du groupe), dans l'ordre —
    /// filtrée des chemins dégénérés (voir `build_lyon_path`), comme
    /// `pdf_render::build_clip_mask` dégrade silencieusement en "pas de
    /// clip" plutôt que de bloquer le rendu. `is_empty()` signifie donc
    /// "pas de clip effectif", que la pile d'origine ait été `None` ou
    /// entièrement dégénérée.
    clip_layers: Vec<VertexBuffers<Vertex, u32>>,
    draws: Vec<Draw>,
}

/// Regroupe les items de `display` en `Group`s consécutifs partageant la
/// même pile de clip (comparaison par pointeur `Rc::as_ptr`, comme
/// `pdf_render::render_to_pixmap`'s `mask_for`) — une nouvelle pile revue
/// plus loin dans la liste (après un `Q` de restauration, par exemple)
/// redéclenche un nouveau groupe plutôt que de réutiliser le premier :
/// correct dans tous les cas, simplement pas optimal si la même pile
/// alterne souvent avec une autre (non observé en pratique, le clip PDF
/// change rarement item par item).
fn group_items(display: &DisplayList, mapper: &PageToNdc) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    let mut current_key: Option<Option<*const Vec<pdf_core::display::ClipPath>>> = None;
    let mut fill_tessellator = FillTessellator::new();
    let mut stroke_tessellator = StrokeTessellator::new();
    let mut glyph_cache: GlyphCache = GlyphCache::new();

    let clip_key = |clip: &Option<ClipStack>| clip.as_ref().map(Rc::as_ptr);

    for item in &display.items {
        let clip = match item {
            DisplayItem::Path { clip, .. }
            | DisplayItem::Glyph { clip, .. }
            | DisplayItem::Image { clip, .. } => clip,
        };
        let key = clip_key(clip);
        if current_key != Some(key) {
            current_key = Some(key);
            let clip_layers = clip
                .as_ref()
                .map(|stack| {
                    stack
                        .iter()
                        .filter_map(|clip_path| {
                            let path = build_lyon_path(&clip_path.segments)?;
                            let mut geometry: VertexBuffers<Vertex, u32> = VertexBuffers::new();
                            let options = FillOptions::default()
                                .with_fill_rule(to_lyon_fill_rule(clip_path.fill_rule));
                            let _ = fill_tessellator.tessellate_path(
                                &path,
                                &options,
                                &mut BuffersBuilder::new(
                                    &mut geometry,
                                    WithColor {
                                        color: [0.0, 0.0, 0.0, 0.0],
                                        mapper: *mapper,
                                    },
                                ),
                            );
                            Some(geometry)
                        })
                        .collect()
                })
                .unwrap_or_default();
            groups.push(Group {
                clip_layers,
                draws: Vec::new(),
            });
        }
        let group = groups.last_mut().expect("just pushed if empty");

        let pending = match group.draws.last_mut() {
            Some(Draw::Solid(geometry)) => geometry,
            _ => {
                group.draws.push(Draw::Solid(VertexBuffers::new()));
                let Some(Draw::Solid(geometry)) = group.draws.last_mut() else {
                    unreachable!()
                };
                geometry
            }
        };

        match item {
            DisplayItem::Path {
                segments,
                paint,
                fill_rule,
                fill_color,
                stroke_color,
                line_width,
                ..
            } => {
                let Some(path) = build_lyon_path(segments) else {
                    continue;
                };
                if matches!(paint, PaintOp::Fill | PaintOp::FillStroke) {
                    let options =
                        FillOptions::default().with_fill_rule(to_lyon_fill_rule(*fill_rule));
                    let _ = fill_tessellator.tessellate_path(
                        &path,
                        &options,
                        &mut BuffersBuilder::new(
                            pending,
                            WithColor {
                                color: to_rgba(*fill_color),
                                mapper: *mapper,
                            },
                        ),
                    );
                }
                if matches!(paint, PaintOp::Stroke | PaintOp::FillStroke) {
                    let options =
                        StrokeOptions::default().with_line_width(line_width.max(0.1) as f32);
                    let _ = stroke_tessellator.tessellate_path(
                        &path,
                        &options,
                        &mut BuffersBuilder::new(
                            pending,
                            WithColor {
                                color: to_rgba(*stroke_color),
                                mapper: *mapper,
                            },
                        ),
                    );
                }
            }
            DisplayItem::Glyph {
                font,
                code,
                outline: Some(segments),
                transform,
                color,
                ..
            } => {
                let cached = glyph_cache
                    .entry((font.clone(), *code))
                    .or_insert_with(|| {
                        let path = build_lyon_path(segments)?;
                        let mut geometry: VertexBuffers<[f32; 2], u32> = VertexBuffers::new();
                        let _ = fill_tessellator.tessellate_path(
                            &path,
                            &FillOptions::default(), // nonzero, correct pour des glyphes.
                            &mut BuffersBuilder::new(&mut geometry, RawPosition),
                        );
                        if geometry.indices.is_empty() {
                            None
                        } else {
                            Some((geometry.vertices, geometry.indices))
                        }
                    });
                if let Some(geometry) = cached.as_ref() {
                    append_cached_glyph(geometry, transform, to_rgba(*color), mapper, pending);
                }
            }
            DisplayItem::Glyph { outline: None, .. } => {}
            DisplayItem::Image {
                pixels: Some(image),
                transform,
                ..
            } => {
                if image.width == 0 || image.height == 0 {
                    continue;
                }
                group.draws.push(Draw::Image {
                    width: image.width,
                    height: image.height,
                    rgba: image.rgba.clone(),
                    quad: build_image_quad(transform, mapper),
                });
            }
            DisplayItem::Image { pixels: None, .. } => {}
        }
    }

    // Une `DisplayList` vide ne produit aucun groupe, mais la page doit
    // quand même être rasterisée en blanc (`rasterize` clippe la couleur
    // au premier groupe) — sans ce groupe placeholder, la texture cible ne
    // serait jamais touchée et resterait à son contenu non initialisé.
    if groups.is_empty() {
        groups.push(Group {
            clip_layers: Vec::new(),
            draws: Vec::new(),
        });
    }

    groups
}

#[derive(Clone)]
struct GpuContext {
    device: std::sync::Arc<wgpu::Device>,
    queue: std::sync::Arc<wgpu::Queue>,
}

impl GpuContext {
    fn new() -> Option<Self> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface: None,
            force_fallback_adapter: false,
        }))?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None))
                .ok()?;
        Some(Self {
            device: std::sync::Arc::new(device),
            queue: std::sync::Arc::new(queue),
        })
    }

    fn render(
        &self,
        display: &DisplayList,
        media_box: [f64; 4],
        rotate: i32,
        width: u32,
        height: u32,
    ) -> Option<RenderedPage> {
        let mapper = PageToNdc {
            origin_x: media_box[0],
            origin_y: media_box[1],
            width_pts: (media_box[2] - media_box[0]).max(1e-6),
            height_pts: (media_box[3] - media_box[1]).max(1e-6),
            rotate,
        };

        let groups = group_items(display, &mapper);
        self.rasterize(&groups, width, height)
    }

    fn rasterize(&self, groups: &[Group], width: u32, height: u32) -> Option<RenderedPage> {
        use wgpu::util::DeviceExt;

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pdf-render-gpu-target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        // Stencil dédié au clip (voir la doc de module) : recréé pour
        // chaque appel comme le reste de ce contexte headless, remis à zéro
        // (`LoadOp::Clear`) à chaque frontière de groupe plutôt qu'une seule
        // fois, pour ne jamais mélanger l'accumulation d'intersection de
        // deux piles de clip différentes.
        let stencil = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pdf-render-gpu-stencil"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Stencil8,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let stencil_view = stencil.create_view(&wgpu::TextureViewDescriptor::default());

        let solid_shader = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("pdf-render-gpu-solid"),
                source: wgpu::ShaderSource::Wgsl(SHADER.into()),
            });
        let image_shader = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("pdf-render-gpu-image"),
                source: wgpu::ShaderSource::Wgsl(IMAGE_SHADER.into()),
            });

        let solid_pipeline_layout =
            self.device
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: None,
                    bind_group_layouts: &[],
                    push_constant_ranges: &[],
                });

        let image_bind_group_layout =
            self.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("pdf-render-gpu-image-bind-group-layout"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                                view_dimension: wgpu::TextureViewDimension::D2,
                                multisampled: false,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                    ],
                });
        let image_pipeline_layout =
            self.device
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: None,
                    bind_group_layouts: &[&image_bind_group_layout],
                    push_constant_ranges: &[],
                });

        // Trois familles de pipelines, chacune en variante "non clippée"
        // (le stencil n'est jamais testé, `Always`) et "clippée" (le
        // fragment n'est peint que là où le stencil vaut la référence
        // dynamique posée par `set_stencil_reference` — la profondeur
        // totale de la pile de clip du groupe courant, voir plus bas) :
        // - `clip_write` : peint les `ClipPath` eux-mêmes dans le stencil
        //   (pas de sortie couleur), une couche à la fois, incrémentée
        //   uniquement là où la couche précédente est déjà passée —
        //   accumule ainsi l'intersection des clips imbriqués.
        // - `content_*` : chemins/glyphes (`SHADER`, couleur pleine).
        // - `image_*` : quads texturés (`IMAGE_SHADER`).
        let no_stencil_test = wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Stencil8,
            depth_write_enabled: false,
            depth_compare: wgpu::CompareFunction::Always,
            stencil: wgpu::StencilState {
                front: wgpu::StencilFaceState {
                    compare: wgpu::CompareFunction::Always,
                    fail_op: wgpu::StencilOperation::Keep,
                    depth_fail_op: wgpu::StencilOperation::Keep,
                    pass_op: wgpu::StencilOperation::Keep,
                },
                back: wgpu::StencilFaceState {
                    compare: wgpu::CompareFunction::Always,
                    fail_op: wgpu::StencilOperation::Keep,
                    depth_fail_op: wgpu::StencilOperation::Keep,
                    pass_op: wgpu::StencilOperation::Keep,
                },
                read_mask: 0,
                write_mask: 0,
            },
            bias: wgpu::DepthBiasState::default(),
        };
        let masked_by_stencil = wgpu::DepthStencilState {
            stencil: wgpu::StencilState {
                front: wgpu::StencilFaceState {
                    compare: wgpu::CompareFunction::Equal,
                    ..no_stencil_test.stencil.front
                },
                back: wgpu::StencilFaceState {
                    compare: wgpu::CompareFunction::Equal,
                    ..no_stencil_test.stencil.back
                },
                read_mask: 0xff,
                write_mask: 0,
            },
            ..no_stencil_test.clone()
        };
        let clip_write_state = wgpu::DepthStencilState {
            stencil: wgpu::StencilState {
                front: wgpu::StencilFaceState {
                    compare: wgpu::CompareFunction::Equal,
                    fail_op: wgpu::StencilOperation::Keep,
                    depth_fail_op: wgpu::StencilOperation::Keep,
                    pass_op: wgpu::StencilOperation::IncrementClamp,
                },
                back: wgpu::StencilFaceState {
                    compare: wgpu::CompareFunction::Equal,
                    fail_op: wgpu::StencilOperation::Keep,
                    depth_fail_op: wgpu::StencilOperation::Keep,
                    pass_op: wgpu::StencilOperation::IncrementClamp,
                },
                read_mask: 0xff,
                write_mask: 0xff,
            },
            ..no_stencil_test.clone()
        };

        let solid_vertex_buffers = [wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x2,
                },
                wgpu::VertexAttribute {
                    offset: 8,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x4,
                },
            ],
        }];
        let image_vertex_buffers = [wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<TexVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x2,
                },
                wgpu::VertexAttribute {
                    offset: 8,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x2,
                },
            ],
        }];

        let make_solid_pipeline = |label: &str,
                                    depth_stencil: wgpu::DepthStencilState,
                                    color_writes: wgpu::ColorWrites| {
            self.device
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(label),
                    layout: Some(&solid_pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &solid_shader,
                        entry_point: "vs_main",
                        compilation_options: Default::default(),
                        buffers: &solid_vertex_buffers,
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &solid_shader,
                        entry_point: "fs_main",
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: wgpu::TextureFormat::Rgba8Unorm,
                            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                            write_mask: color_writes,
                        })],
                    }),
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: Some(depth_stencil),
                    multisample: wgpu::MultisampleState::default(),
                    multiview: None,
                    cache: None,
                })
        };
        let content_pipeline =
            make_solid_pipeline("pdf-render-gpu-content", no_stencil_test.clone(), wgpu::ColorWrites::ALL);
        let content_pipeline_clipped = make_solid_pipeline(
            "pdf-render-gpu-content-clipped",
            masked_by_stencil.clone(),
            wgpu::ColorWrites::ALL,
        );
        let clip_write_pipeline = make_solid_pipeline(
            "pdf-render-gpu-clip-write",
            clip_write_state,
            wgpu::ColorWrites::empty(),
        );

        let make_image_pipeline = |label: &str, depth_stencil: wgpu::DepthStencilState| {
            self.device
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(label),
                    layout: Some(&image_pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &image_shader,
                        entry_point: "vs_main",
                        compilation_options: Default::default(),
                        buffers: &image_vertex_buffers,
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &image_shader,
                        entry_point: "fs_main",
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: wgpu::TextureFormat::Rgba8Unorm,
                            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: Some(depth_stencil),
                    multisample: wgpu::MultisampleState::default(),
                    multiview: None,
                    cache: None,
                })
        };
        let image_pipeline = make_image_pipeline("pdf-render-gpu-image", no_stencil_test);
        let image_pipeline_clipped =
            make_image_pipeline("pdf-render-gpu-image-clipped", masked_by_stencil);

        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

        for (group_index, group) in groups.iter().enumerate() {
            let clip_depth = group.clip_layers.len() as u32;
            let is_clipped = clip_depth > 0;

            let color_load = if group_index == 0 {
                wgpu::LoadOp::Clear(wgpu::Color::WHITE)
            } else {
                wgpu::LoadOp::Load
            };

            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pdf-render-gpu-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: color_load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &stencil_view,
                    depth_ops: None,
                    stencil_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0),
                        store: wgpu::StoreOp::Store,
                    }),
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            for (layer_index, geometry) in group.clip_layers.iter().enumerate() {
                if geometry.indices.is_empty() {
                    continue;
                }
                let vertex_buffer =
                    self.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("pdf-render-gpu-clip-vertices"),
                            contents: bytemuck::cast_slice(&geometry.vertices),
                            usage: wgpu::BufferUsages::VERTEX,
                        });
                let index_buffer =
                    self.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("pdf-render-gpu-clip-indices"),
                            contents: bytemuck::cast_slice(&geometry.indices),
                            usage: wgpu::BufferUsages::INDEX,
                        });
                pass.set_pipeline(&clip_write_pipeline);
                pass.set_stencil_reference(layer_index as u32);
                pass.set_vertex_buffer(0, vertex_buffer.slice(..));
                pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..geometry.indices.len() as u32, 0, 0..1);
            }

            if is_clipped {
                pass.set_stencil_reference(clip_depth);
            }

            for draw in &group.draws {
                match draw {
                    Draw::Solid(geometry) => {
                        if geometry.indices.is_empty() {
                            continue;
                        }
                        let vertex_buffer = self.device.create_buffer_init(
                            &wgpu::util::BufferInitDescriptor {
                                label: Some("pdf-render-gpu-vertices"),
                                contents: bytemuck::cast_slice(&geometry.vertices),
                                usage: wgpu::BufferUsages::VERTEX,
                            },
                        );
                        let index_buffer = self.device.create_buffer_init(
                            &wgpu::util::BufferInitDescriptor {
                                label: Some("pdf-render-gpu-indices"),
                                contents: bytemuck::cast_slice(&geometry.indices),
                                usage: wgpu::BufferUsages::INDEX,
                            },
                        );
                        let pipeline = if is_clipped {
                            &content_pipeline_clipped
                        } else {
                            &content_pipeline
                        };
                        pass.set_pipeline(pipeline);
                        pass.set_vertex_buffer(0, vertex_buffer.slice(..));
                        pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                        pass.draw_indexed(0..geometry.indices.len() as u32, 0, 0..1);
                    }
                    Draw::Image {
                        width: img_w,
                        height: img_h,
                        rgba,
                        quad,
                    } => {
                        let texture = self.device.create_texture_with_data(
                            &self.queue,
                            &wgpu::TextureDescriptor {
                                label: Some("pdf-render-gpu-image-src"),
                                size: wgpu::Extent3d {
                                    width: *img_w,
                                    height: *img_h,
                                    depth_or_array_layers: 1,
                                },
                                mip_level_count: 1,
                                sample_count: 1,
                                dimension: wgpu::TextureDimension::D2,
                                format: wgpu::TextureFormat::Rgba8Unorm,
                                usage: wgpu::TextureUsages::TEXTURE_BINDING
                                    | wgpu::TextureUsages::COPY_DST,
                                view_formats: &[],
                            },
                            wgpu::util::TextureDataOrder::LayerMajor,
                            rgba,
                        );
                        let texture_view =
                            texture.create_view(&wgpu::TextureViewDescriptor::default());
                        let bind_group =
                            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                                label: Some("pdf-render-gpu-image-bind-group"),
                                layout: &image_bind_group_layout,
                                entries: &[
                                    wgpu::BindGroupEntry {
                                        binding: 0,
                                        resource: wgpu::BindingResource::TextureView(
                                            &texture_view,
                                        ),
                                    },
                                    wgpu::BindGroupEntry {
                                        binding: 1,
                                        resource: wgpu::BindingResource::Sampler(&sampler),
                                    },
                                ],
                            });
                        let (vertices, indices) = quad;
                        let vertex_buffer = self.device.create_buffer_init(
                            &wgpu::util::BufferInitDescriptor {
                                label: Some("pdf-render-gpu-image-vertices"),
                                contents: bytemuck::cast_slice(vertices),
                                usage: wgpu::BufferUsages::VERTEX,
                            },
                        );
                        let index_buffer = self.device.create_buffer_init(
                            &wgpu::util::BufferInitDescriptor {
                                label: Some("pdf-render-gpu-image-indices"),
                                contents: bytemuck::cast_slice(indices),
                                usage: wgpu::BufferUsages::INDEX,
                            },
                        );
                        let pipeline = if is_clipped {
                            &image_pipeline_clipped
                        } else {
                            &image_pipeline
                        };
                        pass.set_pipeline(pipeline);
                        pass.set_bind_group(0, &bind_group, &[]);
                        pass.set_vertex_buffer(0, vertex_buffer.slice(..));
                        pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint16);
                        pass.draw_indexed(0..indices.len() as u32, 0, 0..1);
                    }
                }
            }
        }

        // Lecture de la texture : `bytes_per_row` doit être un multiple de
        // `COPY_BYTES_PER_ROW_ALIGNMENT` (256), généralement différent de
        // `width * 4` — d'où le retrait du padding ligne par ligne ci-dessous.
        let unpadded_bytes_per_row = width * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;

        let output_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pdf-render-gpu-readback"),
            size: (padded_bytes_per_row * height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &output_buffer,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));

        let slice = output_buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        self.device.poll(wgpu::Maintain::Wait);
        rx.recv().ok()?.ok()?;

        let data = slice.get_mapped_range();
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for row in 0..height {
            let start = (row * padded_bytes_per_row) as usize;
            let end = start + unpadded_bytes_per_row as usize;
            rgba.extend_from_slice(&data[start..end]);
        }
        drop(data);
        output_buffer.unmap();

        Some(RenderedPage {
            width,
            height,
            rgba,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdf_core::display::{Color, DisplayItem, DisplayList, FillRule, PaintOp, PathSegment};

    fn pixel(page: &RenderedPage, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let i = ((y * page.width + x) * 4) as usize;
        (
            page.rgba[i],
            page.rgba[i + 1],
            page.rgba[i + 2],
            page.rgba[i + 3],
        )
    }

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

    /// `GpuRenderer` doit rester utilisable pour plusieurs pages
    /// successives avec le même `Device`/`Queue` (le cas d'usage visé côté
    /// `pdf-ui`, voir la doc de `GpuRenderer`) — deux appels avec des
    /// couleurs différentes ne doivent pas se mélanger ni paniquer sur un
    /// device réutilisé.
    #[test]
    fn gpu_renderer_renders_multiple_pages_with_the_same_device() {
        let Some(renderer) = GpuRenderer::new() else {
            eprintln!("no wgpu adapter available in this environment, skipping");
            return;
        };

        let red = renderer
            .render_page(&rect_display(Color::Rgb(1.0, 0.0, 0.0)), [0.0, 0.0, 100.0, 100.0])
            .unwrap();
        let blue = renderer
            .render_page(&rect_display(Color::Rgb(0.0, 0.0, 1.0)), [0.0, 0.0, 100.0, 100.0])
            .unwrap();

        let red_center = pixel(&red, 50, 50);
        assert_eq!((red_center.0, red_center.1, red_center.2), (255, 0, 0));
        let blue_center = pixel(&blue, 50, 50);
        assert_eq!((blue_center.0, blue_center.1, blue_center.2), (0, 0, 255));
    }

    /// Symétrique de `pdf_render::tests::renders_filled_rect_with_correct_color_and_flip` :
    /// même fixture, même page, doit produire la même image (aux
    /// différences de rastérisation/anti-aliasing près) — en particulier le
    /// mapping page -> NDC ne doit pas inverser l'axe Y.
    #[test]
    fn renders_filled_rect_at_the_correct_position() {
        let Some(page) = render_page(
            &rect_display(Color::Rgb(1.0, 0.0, 0.0)),
            [0.0, 0.0, 100.0, 100.0],
        ) else {
            eprintln!("no wgpu adapter available in this environment, skipping");
            return;
        };
        assert_eq!((page.width, page.height), (100, 100));

        let center = pixel(&page, 50, 50);
        assert_eq!((center.0, center.1, center.2), (255, 0, 0));

        let outside = pixel(&page, 5, 5);
        assert_eq!((outside.0, outside.1, outside.2), (255, 255, 255));
    }

    /// Place le rectangle uniquement dans le quart **haut-gauche** de la
    /// page (espace PDF : y grand = haut de page) pour vérifier sans
    /// ambiguïté que ce quart atterrit bien en haut de l'image produite
    /// (ligne 0 = haut, convention raster standard) — c'est le test qui
    /// validerait un mapping Y inversé par erreur.
    #[test]
    fn top_left_page_content_lands_in_the_top_left_of_the_image() {
        let display = DisplayList {
            items: vec![DisplayItem::Path {
                segments: vec![
                    PathSegment::MoveTo((0.0, 70.0)),
                    PathSegment::LineTo((30.0, 70.0)),
                    PathSegment::LineTo((30.0, 100.0)),
                    PathSegment::LineTo((0.0, 100.0)),
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
        let Some(page) = render_page(&display, [0.0, 0.0, 100.0, 100.0]) else {
            eprintln!("no wgpu adapter available in this environment, skipping");
            return;
        };

        let top_left = pixel(&page, 10, 10);
        assert_eq!(
            (top_left.0, top_left.1, top_left.2),
            (0, 0, 0),
            "content near page-space y=100 (top) should render near image row 0 (top)"
        );
        let bottom_right = pixel(&page, 90, 90);
        assert_eq!(
            (bottom_right.0, bottom_right.1, bottom_right.2),
            (255, 255, 255)
        );
    }

    #[test]
    fn empty_display_list_produces_white_page() {
        let Some(page) = render_page(&DisplayList::default(), [0.0, 0.0, 20.0, 20.0]) else {
            eprintln!("no wgpu adapter available in this environment, skipping");
            return;
        };
        let px = pixel(&page, 10, 10);
        assert_eq!((px.0, px.1, px.2), (255, 255, 255));
    }

    /// Bout en bout sur un vrai PDF avec police TrueType intégrée, comme
    /// `pdf-render::tests::renders_real_embedded_font_glyphs` — vérifie que
    /// la tessellation des contours de glyphes fonctionne, pas seulement les
    /// rectangles synthétiques ci-dessus.
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

        let Some(rendered) = render_page(&display, page.media_box) else {
            eprintln!("no wgpu adapter available in this environment, skipping");
            return;
        };
        let has_non_white_pixel = (0..rendered.width).any(|x| {
            (0..rendered.height).any(|y| {
                let (r, g, b, _) = pixel(&rendered, x, y);
                (r, g, b) != (255, 255, 255)
            })
        });
        assert!(
            has_non_white_pixel,
            "expected glyph ink somewhere on the page"
        );
    }

    /// Deux occurrences du même glyphe (même `font`/`code`, donc la seconde
    /// est servie par `GlyphCache` plutôt que re-tessellée) à deux positions
    /// distinctes doivent chacune peindre à leur propre endroit — preuve que
    /// `append_cached_glyph` applique bien la matrice `transform` *par
    /// occurrence* plutôt que de rejouer la position mise en cache lors de
    /// la première tessellation.
    #[test]
    fn repeated_glyph_renders_at_each_occurrence_position_via_cache() {
        let outline = vec![
            PathSegment::MoveTo((0.0, 0.0)),
            PathSegment::LineTo((10.0, 0.0)),
            PathSegment::LineTo((10.0, 10.0)),
            PathSegment::LineTo((0.0, 10.0)),
            PathSegment::ClosePath,
        ];
        let glyph_at = |tx: f64, ty: f64| DisplayItem::Glyph {
            font: "F1".into(),
            code: 65,
            unicode: Some('A'),
            transform: Matrix::translation(tx, ty),
            color: Color::Gray(0.0),
            advance_is_estimated: false,
            outline: Some(outline.clone()),
            clip: None,
        };
        let display = DisplayList {
            items: vec![glyph_at(10.0, 10.0), glyph_at(70.0, 70.0)],
        };
        let Some(page) = render_page(&display, [0.0, 0.0, 100.0, 100.0]) else {
            eprintln!("no wgpu adapter available in this environment, skipping");
            return;
        };

        let first = pixel(&page, 15, 100 - 15);
        assert_eq!((first.0, first.1, first.2), (0, 0, 0));
        let second = pixel(&page, 75, 100 - 75);
        assert_eq!((second.0, second.1, second.2), (0, 0, 0));
        let between = pixel(&page, 50, 50);
        assert_eq!(
            (between.0, between.1, between.2),
            (255, 255, 255),
            "the cached glyph must not also paint at the first occurrence's position"
        );
    }

    /// Un chemin tracé (pas rempli) doit peindre une fine bande le long du
    /// contour, pas l'intérieur — preuve que le trait est réellement
    /// tessellé (`StrokeTessellator`), pas seulement ignoré comme dans la
    /// première tranche.
    #[test]
    fn strokes_a_path_without_filling_its_interior() {
        let display = DisplayList {
            items: vec![DisplayItem::Path {
                segments: vec![
                    PathSegment::MoveTo((10.0, 10.0)),
                    PathSegment::LineTo((90.0, 10.0)),
                    PathSegment::LineTo((90.0, 90.0)),
                    PathSegment::LineTo((10.0, 90.0)),
                    PathSegment::ClosePath,
                ],
                paint: PaintOp::Stroke,
                fill_rule: FillRule::NonZero,
                fill_color: Color::Gray(0.0),
                stroke_color: Color::Rgb(0.0, 0.0, 1.0),
                line_width: 4.0,
                sets_clip: false,
                clip: None,
            }],
        };
        let Some(page) = render_page(&display, [0.0, 0.0, 100.0, 100.0]) else {
            eprintln!("no wgpu adapter available in this environment, skipping");
            return;
        };

        // Sur le bord tracé (x=10) : bleu.
        let on_stroke = pixel(&page, 10, 50);
        assert_eq!((on_stroke.0, on_stroke.1, on_stroke.2), (0, 0, 255));

        // Au centre du rectangle (jamais rempli, seulement tracé) : blanc.
        let interior = pixel(&page, 50, 50);
        assert_eq!(
            (interior.0, interior.1, interior.2),
            (255, 255, 255),
            "stroke-only path must not fill its interior"
        );
    }

    /// `render_page_rotated(rotate=0)` doit produire les mêmes dimensions
    /// que `render_page` — la normalisation ne doit rien changer pour la
    /// valeur la plus courante.
    #[test]
    fn rotate_zero_matches_unrotated_dimensions() {
        let display = rect_display(Color::Rgb(1.0, 0.0, 0.0));
        let Some(plain) = render_page(&display, [0.0, 0.0, 100.0, 60.0]) else {
            eprintln!("no wgpu adapter available in this environment, skipping");
            return;
        };
        let rotated = render_page_rotated(&display, [0.0, 0.0, 100.0, 60.0], 0, 1.0).unwrap();
        assert_eq!((plain.width, plain.height), (100, 60));
        assert_eq!((rotated.width, rotated.height), (100, 60));
    }

    /// `/Rotate 90` doit permuter largeur/hauteur — même comportement que
    /// `pdf_render::render_page_rotated`.
    #[test]
    fn rotate_90_swaps_dimensions() {
        let display = rect_display(Color::Rgb(1.0, 0.0, 0.0));
        let Some(page) = render_page_rotated(&display, [0.0, 0.0, 100.0, 60.0], 90, 1.0) else {
            eprintln!("no wgpu adapter available in this environment, skipping");
            return;
        };
        assert_eq!((page.width, page.height), (60, 100));
    }

    /// Un carré placé dans le coin haut-gauche de la page doit atterrir dans
    /// le coin haut-**droit** de l'image après une rotation `/Rotate 90`
    /// (rotation horaire vue du lecteur) — même dérivation que côté CPU
    /// (`pdf_render::tests::rotate_90_moves_bottom_left_content_to_top_left`,
    /// adaptée ici au coin haut-gauche car ce backend ne flip pas Y).
    #[test]
    fn rotate_90_moves_top_left_content_to_top_right() {
        let display = DisplayList {
            items: vec![DisplayItem::Path {
                segments: vec![
                    PathSegment::MoveTo((0.0, 80.0)),
                    PathSegment::LineTo((20.0, 80.0)),
                    PathSegment::LineTo((20.0, 100.0)),
                    PathSegment::LineTo((0.0, 100.0)),
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
        let Some(page) = render_page_rotated(&display, [0.0, 0.0, 100.0, 100.0], 90, 1.0) else {
            eprintln!("no wgpu adapter available in this environment, skipping");
            return;
        };
        assert_eq!((page.width, page.height), (100, 100));

        let top_right = pixel(&page, 90, 10);
        assert_eq!(
            (top_right.0, top_right.1, top_right.2),
            (0, 0, 0),
            "top-left page content should land top-right after a 90° rotation"
        );
        let bottom_left = pixel(&page, 10, 90);
        assert_eq!(
            (bottom_left.0, bottom_left.1, bottom_left.2),
            (255, 255, 255)
        );
    }

    /// Symétrique de `pdf_render::tests::draws_decoded_image_at_correct_position` :
    /// image bicolore (rouge à gauche, bleu à droite) occupant toute la
    /// page — preuve que le quad texturé (`build_image_quad`) est bien
    /// placé et que le flip pixel -> carré unité ne mélange pas les
    /// moitiés gauche/droite ni ne les inverse verticalement.
    #[test]
    fn draws_decoded_image_at_correct_position() {
        use pdf_core::display::DecodedImage;

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
        let Some(page) = render_page(&display, [0.0, 0.0, 100.0, 100.0]) else {
            eprintln!("no wgpu adapter available in this environment, skipping");
            return;
        };

        let left = pixel(&page, 25, 50);
        assert_eq!((left.0, left.1, left.2), (255, 0, 0));
        let right = pixel(&page, 75, 50);
        assert_eq!((right.0, right.1, right.2), (0, 0, 255));
    }

    /// Symétrique de `pdf_render::tests::semi_transparent_image_blends_with_page_background` :
    /// une image rouge à ~50% d'alpha peinte sur le fond blanc doit se
    /// fondre avec lui (rose), pas apparaître en rouge plein comme si
    /// l'alpha du `DecodedImage` était ignoré par le shader de texture.
    #[test]
    fn semi_transparent_image_blends_with_page_background() {
        use pdf_core::display::DecodedImage;

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
        let Some(page) = render_page(&display, [0.0, 0.0, 100.0, 100.0]) else {
            eprintln!("no wgpu adapter available in this environment, skipping");
            return;
        };
        let center = pixel(&page, 50, 50);
        // Ni blanc pur (alpha ignoré à 0) ni rouge pur (alpha ignoré à 255) :
        // un mélange, avec un vert/bleu résiduel du fond blanc.
        assert!(center.1 > 0 && center.1 < 255);
        assert_eq!(center.1, center.2);
        assert!(center.0 > center.1);
    }

    /// Symétrique de `pdf_render::tests::clip_restricts_painted_area_to_the_clip_path` :
    /// un rectangle noir couvrant toute la page, mais peint sous un clip
    /// (`W`/`W*`, `DisplayItem::Path::clip`) restreint à `10..50` — seule
    /// cette zone doit être noire, le reste doit rester blanc — preuve que
    /// le stencil buffer (voir la doc de module) est réellement appliqué,
    /// pas juste porté par le `DisplayItem` sans effet.
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
        let Some(page) = render_page(&display, [0.0, 0.0, 100.0, 100.0]) else {
            eprintln!("no wgpu adapter available in this environment, skipping");
            return;
        };

        // Centre de la zone de clip (10..50, 10..50) -> doit être peint noir.
        let inside = pixel(&page, 30, 100 - 30);
        assert_eq!((inside.0, inside.1, inside.2), (0, 0, 0));

        // Hors du clip -> doit être resté blanc malgré le rectangle plein page.
        let outside = pixel(&page, 80, 100 - 80);
        assert_eq!((outside.0, outside.1, outside.2), (255, 255, 255));
    }

    /// Deux clips consécutifs mais disjoints (chacun sa propre pile `Rc`,
    /// comme deux `q ... W n ... Q` successifs) doivent chacun restreindre
    /// leur propre item — preuve que le stencil est bien remis à zéro à
    /// chaque frontière de groupe (voir `Group`/`GpuContext::rasterize`)
    /// plutôt que de laisser le stencil du premier clip "fuiter" sur le
    /// second (ce qui, avec un stencil jamais nettoyé, ferait échouer le
    /// test stencil `Equal` du second groupe puisque son compteur partirait
    /// d'une valeur déjà non nulle).
    #[test]
    fn two_sequential_disjoint_clips_each_restrict_their_own_item() {
        use pdf_core::display::ClipPath;

        let clip_a = Some(Rc::new(vec![ClipPath {
            segments: vec![
                PathSegment::MoveTo((0.0, 0.0)),
                PathSegment::LineTo((40.0, 0.0)),
                PathSegment::LineTo((40.0, 100.0)),
                PathSegment::LineTo((0.0, 100.0)),
                PathSegment::ClosePath,
            ],
            fill_rule: FillRule::NonZero,
        }]));
        let clip_b = Some(Rc::new(vec![ClipPath {
            segments: vec![
                PathSegment::MoveTo((60.0, 0.0)),
                PathSegment::LineTo((100.0, 0.0)),
                PathSegment::LineTo((100.0, 100.0)),
                PathSegment::LineTo((60.0, 100.0)),
                PathSegment::ClosePath,
            ],
            fill_rule: FillRule::NonZero,
        }]));

        let full_page_rect = |clip: Option<ClipStack>| DisplayItem::Path {
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
        };

        let display = DisplayList {
            items: vec![full_page_rect(clip_a), full_page_rect(clip_b)],
        };
        let Some(page) = render_page(&display, [0.0, 0.0, 100.0, 100.0]) else {
            eprintln!("no wgpu adapter available in this environment, skipping");
            return;
        };

        let in_a = pixel(&page, 20, 50);
        assert_eq!((in_a.0, in_a.1, in_a.2), (0, 0, 0));
        let in_b = pixel(&page, 80, 50);
        assert_eq!((in_b.0, in_b.1, in_b.2), (0, 0, 0));
        let between = pixel(&page, 50, 50);
        assert_eq!((between.0, between.1, between.2), (255, 255, 255));
    }
}
