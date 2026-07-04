//! Backend de rendu GPU (`wgpu` + tessellation `lyon`) — Sprint 9-10,
//! alternative au backend CPU `tiny-skia` de `pdf-render`.
//!
//! Deuxième tranche, toujours volontairement limitée par rapport à
//! `pdf-render` :
//! - Chemins **remplis** (`PaintOp::Fill`/`FillStroke`) et **tracés**
//!   (`PaintOp::Stroke`/`FillStroke`), et glyphes (toujours remplis, règle
//!   nonzero), tessellés via `lyon::tessellation::{FillTessellator,
//!   StrokeTessellator}`.
//! - Rotation de page (`/Rotate`, voir `render_page_rotated`) appliquée
//!   directement dans l'espace NDC (voir `PageToNdc::map_to_ndc`) plutôt
//!   qu'en pixels comme côté CPU — la structure mathématique est identique
//!   à `pdf_render::rotation_matrix`, seul l'espace de travail change.
//! - **Pas d'images**, **pas de clip** (`W`/`W*`) — existent déjà côté
//!   `pdf-render`/CPU, qui reste la référence tant que ce backend n'a pas
//!   atteint la parité.
//! - Rendu hors-écran uniquement (pas encore branché sur une fenêtre/surface
//!   `pdf-ui`) : `render_page`/`render_page_scaled`/`render_page_rotated`
//!   créent un contexte `wgpu` headless (`Instance::request_adapter` sans
//!   surface) à chaque appel, rastérisent dans une texture, et relisent le
//!   résultat en RGBA8 — suffisant pour valider le pipeline par comparaison
//!   avec `pdf-render`, pas optimisé pour un rendu interactif (recréer le
//!   device à chaque page serait couteux dans une vraie boucle de rendu).
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
use pdf_core::display::{Color, DisplayItem, DisplayList, FillRule, Matrix, PaintOp, PathSegment};

/// Image RGBA8 rastérisée par ce backend — même forme que
/// `pdf_app::RenderedPage`/le pixmap de `pdf-render`, pour rester
/// interchangeable côté appelant.
pub struct RenderedPage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Rastérise une page entière (voir les limitations en tête de module).
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
/// (`/Rotate`, ISO 32000-1 §7.7.3.3). Les dimensions du pixmap sont
/// permutées pour 90°/270° (portrait -> paysage), comme côté
/// `pdf_render::render_page_rotated`.
pub fn render_page_rotated(
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

    let ctx = GpuContext::new()?;
    ctx.render(display, media_box, rotate, width, height)
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

/// Applique `transform` (échelle police + matrice texte + CTM, voir
/// `pdf_core::interp`) à des segments en espace em, pour les ramener en
/// espace page — même rôle que `pdf-render::build_glyph_path`, mais on
/// produit ici des `PathSegment` plutôt que directement des primitives
/// `lyon`, pour réutiliser `build_lyon_path` sans dupliquer sa logique.
fn map_segments_to_page_space(segments: &[PathSegment], transform: &Matrix) -> Vec<PathSegment> {
    let map = |p: (f64, f64)| transform.apply(p.0, p.1);
    segments
        .iter()
        .map(|seg| match seg {
            PathSegment::MoveTo(p) => PathSegment::MoveTo(map(*p)),
            PathSegment::LineTo(p) => PathSegment::LineTo(map(*p)),
            PathSegment::CurveTo { c1, c2, to } => PathSegment::CurveTo {
                c1: map(*c1),
                c2: map(*c2),
                to: map(*to),
            },
            PathSegment::ClosePath => PathSegment::ClosePath,
        })
        .collect()
}

struct GpuContext {
    device: wgpu::Device,
    queue: wgpu::Queue,
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
        Some(Self { device, queue })
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

        let mut geometry: VertexBuffers<Vertex, u32> = VertexBuffers::new();
        let mut fill_tessellator = FillTessellator::new();
        let mut stroke_tessellator = StrokeTessellator::new();

        for item in &display.items {
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
                                &mut geometry,
                                WithColor {
                                    color: to_rgba(*fill_color),
                                    mapper,
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
                                &mut geometry,
                                WithColor {
                                    color: to_rgba(*stroke_color),
                                    mapper,
                                },
                            ),
                        );
                    }
                }
                DisplayItem::Glyph {
                    outline: Some(segments),
                    transform,
                    color,
                    ..
                } => {
                    let mapped = map_segments_to_page_space(segments, transform);
                    let Some(path) = build_lyon_path(&mapped) else {
                        continue;
                    };
                    let _ = fill_tessellator.tessellate_path(
                        &path,
                        &FillOptions::default(), // nonzero, correct pour des glyphes.
                        &mut BuffersBuilder::new(
                            &mut geometry,
                            WithColor {
                                color: to_rgba(*color),
                                mapper,
                            },
                        ),
                    );
                }
                // Images, glyphes sans contour, clip : voir limitations en
                // tête de module.
                _ => {}
            }
        }

        self.rasterize(&geometry, width, height)
    }

    fn rasterize(
        &self,
        geometry: &VertexBuffers<Vertex, u32>,
        width: u32,
        height: u32,
    ) -> Option<RenderedPage> {
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

        let shader = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("pdf-render-gpu-solid"),
                source: wgpu::ShaderSource::Wgsl(SHADER.into()),
            });
        let pipeline_layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[],
                push_constant_ranges: &[],
            });
        let pipeline = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("pdf-render-gpu-fill"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: "vs_main",
                    compilation_options: Default::default(),
                    buffers: &[wgpu::VertexBufferLayout {
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
                    }],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: "fs_main",
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            });

        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("pdf-render-gpu-vertices"),
                contents: bytemuck::cast_slice(&geometry.vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("pdf-render-gpu-indices"),
                contents: bytemuck::cast_slice(&geometry.indices),
                usage: wgpu::BufferUsages::INDEX,
            });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pdf-render-gpu-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if !geometry.indices.is_empty() {
                pass.set_pipeline(&pipeline);
                pass.set_vertex_buffer(0, vertex_buffer.slice(..));
                pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..geometry.indices.len() as u32, 0, 0..1);
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
}
