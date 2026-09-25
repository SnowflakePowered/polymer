use crate::wii::WiiBanner;
use bannerbobomb_parser::brlyt::{
    Material, Pane, PaneKind, Picture, TevStage, TextBox, TextureSrt, Window,
};
use bannerbobomb_parser::timeline::{MaterialState, PaneState};
use bytemuck::{Pod, Zeroable};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Instant;

const SAMPLES: u32 = 4;
const COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const TEXTURE_SLOTS: usize = 8;
const MAX_STAGES: usize = 16;
/// Uniform stride per draw; a multiple of every backend's offset alignment.
const MATERIAL_STRIDE: u64 = 2048;
const FRAMES_PER_SECOND: f32 = 60.0;
/// GX texture coordinate generator matrices start at `GX_TEXMTX0` in steps of three.
const TEXTURE_MATRIX_BASE: u8 = 30;
/// GX texture coordinate generator sources start at `GX_TG_TEX0`.
const TEXTURE_COORDINATE_BASE: u8 = 4;
const NO_TEXTURE: u32 = 0xFF;
/// GX_COLOR0A0, the rasterized vertex color channel.
const RASTER_COLOR: u8 = 4;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    position: [f32; 2],
    color: [f32; 4],
    uv: [[f32; 2]; TEXTURE_SLOTS],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Stage {
    color: [u32; 4],
    alpha: [u32; 4],
    config: [u32; 4],
    indirect: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MaterialUniform {
    registers: [[f32; 4]; 4],
    constants: [[f32; 4]; 4],
    swap_tables: [[u32; 4]; 4],
    info: [u32; 4],
    indirect_orders: [u32; 4],
    indirect_matrices: [[f32; 4]; 6],
    stages: [Stage; MAX_STAGES],
}

impl Stage {
    /// Operation, bias, scale, clamp, output register, and constant selection
    /// packed as the shader reads them.
    fn operation(
        operation: u8,
        bias: u8,
        scale: u8,
        clamp: bool,
        register: u8,
        constant: u8,
    ) -> u32 {
        u32::from(operation & 0xF)
            | u32::from(bias & 3) << 4
            | u32::from(scale & 3) << 6
            | u32::from(clamp) << 8
            | u32::from(register & 3) << 9
            | u32::from(constant & 0x1F) << 11
    }

    fn new(stage: &TevStage) -> Self {
        Self {
            color: stage.color_inputs.map(u32::from),
            alpha: stage.alpha_inputs.map(u32::from),
            config: [
                u32::from(stage.texture_coordinate)
                    | u32::from(stage.raster_color) << 8
                    | u32::from(stage.raster_swap) << 16
                    | u32::from(stage.texture_swap) << 24,
                if stage.texture_map == 0xFF {
                    NO_TEXTURE
                } else {
                    u32::from(stage.texture_map)
                },
                Self::operation(
                    stage.color_operation,
                    stage.color_bias,
                    stage.color_scale,
                    stage.color_clamp,
                    stage.color_register,
                    stage.color_constant,
                ),
                Self::operation(
                    stage.alpha_operation,
                    stage.alpha_bias,
                    stage.alpha_scale,
                    stage.alpha_clamp,
                    stage.alpha_register,
                    stage.alpha_constant,
                ),
            ],
            indirect: [
                u32::from(stage.indirect_texture & 3)
                    | u32::from(stage.indirect_format & 3) << 4
                    | u32::from(stage.indirect_bias & 7) << 8
                    | u32::from(stage.indirect_matrix & 0xF) << 12
                    | u32::from(stage.indirect_wrap_s & 0xF) << 16
                    | u32::from(stage.indirect_wrap_t & 0xF) << 20
                    | u32::from(stage.indirect_add_previous) << 24,
                0,
                0,
                0,
            ],
        }
    }

    /// NW4R's default combiner for materials without TEV stages: blend between
    /// the black and white registers by the texture, then modulate by the
    /// rasterized color.
    fn defaults(textured: bool) -> Vec<Self> {
        const ZERO_COLOR: u32 = 15;
        const ZERO_ALPHA: u32 = 7;
        let pass = Self::operation(0, 0, 0, true, 0, 0);
        let modulate = Self {
            color: [ZERO_COLOR, 0, 10, ZERO_COLOR],
            alpha: [ZERO_ALPHA, 0, 5, ZERO_ALPHA],
            config: [u32::from(RASTER_COLOR) << 8, NO_TEXTURE, pass, pass],
            indirect: [0; 4],
        };
        if !textured {
            return vec![Self {
                color: [ZERO_COLOR, ZERO_COLOR, ZERO_COLOR, 10],
                alpha: [ZERO_ALPHA, ZERO_ALPHA, ZERO_ALPHA, 5],
                ..modulate
            }];
        }
        vec![
            Self {
                color: [2, 4, 8, ZERO_COLOR],
                alpha: [1, 2, 4, ZERO_ALPHA],
                config: [0xFF << 8, 0, pass, pass],
                indirect: [0; 4],
            },
            modulate,
        ]
    }
}

impl MaterialUniform {
    /// `textured` selects the default combiner for a bound texture when the
    /// material has no stages, as text panes bind their font sheet.
    fn new(material: &Material, state: &MaterialState, textured: bool) -> Self {
        let register = |color: [f32; 4]| color.map(|value| value / 255.0);
        let stages = if material.tev_stages.is_empty() {
            Stage::defaults(textured || !material.texture_references.is_empty())
        } else {
            material
                .tev_stages
                .iter()
                .take(MAX_STAGES)
                .map(Stage::new)
                .collect()
        };
        let mut uniform = Self::zeroed();
        uniform.registers = [
            [0.0; 4],
            register(state.foreground_color),
            register(state.background_color),
            register(state.color_register_3),
        ];
        uniform.constants = state.constant_colors.map(register);
        uniform.swap_tables = material.tev_swap_table.map_or([[0, 1, 2, 3]; 4], |table| {
            table.tables.map(|row| row.map(u32::from))
        });
        uniform.info[0] = stages.len() as u32;
        if let Some(compare) = material.alpha_compare {
            uniform.info[1] = u32::from(compare.comparison_0)
                | u32::from(compare.comparison_1) << 8
                | u32::from(compare.operation) << 16;
            uniform.info[2] = u32::from(compare.reference_0) | u32::from(compare.reference_1) << 8;
            uniform.info[3] = 1;
        }
        for (index, order) in material.indirect_texture_orders.iter().take(4).enumerate() {
            uniform.indirect_orders[index] = u32::from(order.texture_coordinate)
                | u32::from(order.texture_map) << 8
                | u32::from(order.scale_s) << 16
                | u32::from(order.scale_t) << 24;
        }
        for (index, srt) in state.indirect_texture_srts.iter().take(3).enumerate() {
            let (sin, cos) = srt.rotation.to_radians().sin_cos();
            uniform.indirect_matrices[index * 2] = [
                srt.scale[0] * cos,
                -srt.scale[1] * sin,
                srt.translation[0],
                0.0,
            ];
            uniform.indirect_matrices[index * 2 + 1] = [
                srt.scale[0] * sin,
                srt.scale[1] * cos,
                srt.translation[1],
                0.0,
            ];
        }
        uniform.stages[..stages.len()].copy_from_slice(&stages);
        uniform
    }
}

/// GX blend configuration, which selects a render pipeline.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Blend {
    blend_type: u8,
    source: u8,
    destination: u8,
}

impl Blend {
    /// GX_BM_BLEND with source alpha, the NW4R default.
    const DEFAULT: Self = Self {
        blend_type: 1,
        source: 4,
        destination: 5,
    };

    fn new(material: &Material) -> Self {
        material.blend_mode.map_or(Self::DEFAULT, |blend| Self {
            blend_type: blend.blend_type,
            source: blend.source_factor,
            destination: blend.destination_factor,
        })
    }

    /// GX factors, where "color" means the other side's color.
    fn factor(value: u8, source: bool) -> wgpu::BlendFactor {
        use wgpu::BlendFactor::*;
        match (value, source) {
            (0, _) => Zero,
            (1, _) => One,
            (2, true) => Dst,
            (2, false) => Src,
            (3, true) => OneMinusDst,
            (3, false) => OneMinusSrc,
            (4, _) => SrcAlpha,
            (5, _) => OneMinusSrcAlpha,
            (6, _) => DstAlpha,
            _ => OneMinusDstAlpha,
        }
    }

    /// Output is straight alpha drawn into a premultiplied target, so alpha
    /// always accumulates as "over".
    fn state(self) -> wgpu::BlendState {
        let alpha = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        };
        let color = match self.blend_type {
            0 => wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::Zero,
                operation: wgpu::BlendOperation::Add,
            },
            3 => wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::ReverseSubtract,
            },
            1 => wgpu::BlendComponent {
                src_factor: Self::factor(self.source, true),
                dst_factor: Self::factor(self.destination, false),
                operation: wgpu::BlendOperation::Add,
            },
            _ => Self::DEFAULT.state().color,
        };
        wgpu::BlendState { color, alpha }
    }
}

/// A texture slot's GPU texture and GX wrap modes.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Slot {
    texture: Option<usize>,
    wrap: [u8; 2],
}

struct Draw {
    blend: Blend,
    slots: [Slot; TEXTURE_SLOTS],
    vertices: std::ops::Range<u32>,
}

/// A quad: corners in clip space (top-left, top-right, bottom-left,
/// bottom-right), corner colors, texture coordinate sets, and inherited alpha.
struct Quad<'a> {
    corners: [[f32; 2]; 4],
    colors: [[f32; 4]; 4],
    coordinates: Cow<'a, [[[f32; 2]; 4]]>,
    alpha: f32,
}

/// One frame's quads and the per-draw material state they use.
#[derive(Default)]
struct Frame {
    vertices: Vec<Vertex>,
    materials: Vec<MaterialUniform>,
    draws: Vec<Draw>,
}

/// An affine 2D transform `[a, b, c, d, tx, ty]` mapping (x, y) to
/// (a·x + c·y + tx, b·x + d·y + ty).
type Affine = [f32; 6];

/// Where a pane draws: its banner and frame, its transform to layout space
/// with y pointing down, and its inherited alpha.
struct Placement<'a> {
    banner: &'a WiiBanner,
    textures: &'a HashMap<String, usize>,
    frame: f32,
    transform: Affine,
    alpha: f32,
}

impl Placement<'_> {
    fn child(&self, state: &PaneState) -> Self {
        let [(sin_x, cos_x), (sin_y, cos_y), (sin_z, cos_z)] =
            state.rotation.map(|angle| angle.to_radians().sin_cos());
        let [scale_x, scale_y] = state.scale;
        let m00 = cos_z * cos_y * scale_x;
        let m01 = (cos_z * sin_y * sin_x - sin_z * cos_x) * scale_y;
        let m10 = sin_z * cos_y * scale_x;
        let m11 = (sin_z * sin_y * sin_x + cos_z * cos_x) * scale_y;
        let local = [
            m00,
            -m10,
            -m01,
            m11,
            state.translation[0],
            -state.translation[1],
        ];
        Self {
            banner: self.banner,
            textures: self.textures,
            frame: self.frame,
            transform: Frame::multiply(&self.transform, &local),
            alpha: self.alpha * (state.alpha / 255.0).clamp(0.0, 1.0),
        }
    }

    /// A pane-local rectangle `[x, y, width, height]` as a quad.
    fn quad<'c>(
        &self,
        [x, y, width, height]: [f32; 4],
        colors: [[f32; 4]; 4],
        coordinates: Cow<'c, [[[f32; 2]; 4]]>,
    ) -> Quad<'c> {
        let [view_x, view_y, view_width, view_height] = self.banner.viewport;
        let corners = [
            [x, y],
            [x + width, y],
            [x, y + height],
            [x + width, y + height],
        ]
        .map(|corner| {
            let [px, py] = Frame::apply(&self.transform, corner);
            [
                (px - view_x) / view_width * 2.0 - 1.0,
                1.0 - (py - view_y) / view_height * 2.0,
            ]
        });
        Quad {
            corners,
            colors,
            coordinates,
            alpha: self.alpha,
        }
    }

    /// The size of a material's first texture, which sizes window frames.
    fn texture_size(&self, material: u16) -> [f32; 2] {
        let layout = &self.banner.layout;
        layout
            .materials
            .get(usize::from(material))
            .and_then(|material| material.texture_references.first())
            .and_then(|reference| layout.textures.get(usize::from(reference.texture_index)))
            .and_then(|name| self.banner.textures.get(&name.to_ascii_lowercase()))
            .map_or([0.0; 2], |image| {
                [f32::from(image.width), f32::from(image.height)]
            })
    }
}

/// One piece of a window frame: the frame drawn, its rectangle, the corner its
/// texture is anchored to, whether the texture stretches from that corner
/// along the piece, and its GX texture flip.
struct FramePiece {
    frame: usize,
    rect: [f32; 4],
    anchor: usize,
    stretch: bool,
    flip: u8,
}

impl FramePiece {
    /// Texture coordinates measured from the anchored corner in texels, so a
    /// clamped texture's inner edge stretches along the piece, then flipped.
    fn coordinates(&self, [texture_width, texture_height]: [f32; 2]) -> [[f32; 2]; 4] {
        let span = if self.stretch {
            [
                self.rect[2] / texture_width.max(1.0),
                self.rect[3] / texture_height.max(1.0),
            ]
        } else {
            [1.0; 2]
        };
        let u = if self.anchor % 2 == 1 {
            [1.0 - span[0], 1.0]
        } else {
            [0.0, span[0]]
        };
        let v = if self.anchor / 2 == 1 {
            [1.0 - span[1], 1.0]
        } else {
            [0.0, span[1]]
        };
        let corners = [[u[0], v[0]], [u[1], v[0]], [u[0], v[1]], [u[1], v[1]]];
        match self.flip {
            1 => corners.map(|[u, v]| [1.0 - u, v]),
            2 => corners.map(|[u, v]| [u, 1.0 - v]),
            3 => [corners[2], corners[0], corners[3], corners[1]],
            4 => corners.map(|[u, v]| [1.0 - u, 1.0 - v]),
            5 => [corners[1], corners[3], corners[0], corners[2]],
            _ => corners,
        }
    }
}

impl Frame {
    const IDENTITY: Affine = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    const WHITE: [[f32; 4]; 4] = [[255.0; 4]; 4];

    fn multiply(parent: &Affine, child: &Affine) -> Affine {
        [
            parent[0] * child[0] + parent[2] * child[1],
            parent[1] * child[0] + parent[3] * child[1],
            parent[0] * child[2] + parent[2] * child[3],
            parent[1] * child[2] + parent[3] * child[3],
            parent[0] * child[4] + parent[2] * child[5] + parent[4],
            parent[1] * child[4] + parent[3] * child[5] + parent[5],
        ]
    }

    fn apply(transform: &Affine, [x, y]: [f32; 2]) -> [f32; 2] {
        [
            transform[0] * x + transform[2] * y + transform[4],
            transform[1] * x + transform[3] * y + transform[5],
        ]
    }

    /// Lay out every visible pane at `frame`, in draw order.
    fn build(banner: &WiiBanner, textures: &HashMap<String, usize>, frame: f32) -> Self {
        let mut built = Self::default();
        let root = Placement {
            banner,
            textures,
            frame,
            transform: Self::IDENTITY,
            alpha: 1.0,
        };
        for pane in &banner.layout.panes {
            built.pane(&root, pane);
        }
        built
    }

    fn pane(&mut self, parent: &Placement<'_>, pane: &Pane) {
        let state = parent.banner.timeline.pane(pane, parent.frame);
        if !state.visible || !parent.banner.shows(pane) {
            return;
        }
        let placement = parent.child(&state);
        let [width, height] = state.size;
        let anchor = |axis: u8| f32::from(axis.min(2)) * 0.5;
        let rect = [
            -width * anchor(pane.origin % 3),
            -height * anchor(pane.origin / 3),
            width,
            height,
        ];
        let colors = state.vertex_colors.unwrap_or(Self::WHITE);
        match &pane.kind {
            PaneKind::Picture(picture) => self.picture(&placement, rect, picture, colors),
            PaneKind::Window(window) => self.window(&placement, rect, window, colors),
            PaneKind::Text(text) => self.text(&placement, rect, text, colors),
            _ => {}
        }
        for child in &pane.children {
            self.pane(&placement, child);
        }
    }

    fn picture(
        &mut self,
        placement: &Placement<'_>,
        rect: [f32; 4],
        picture: &Picture,
        colors: [[f32; 4]; 4],
    ) {
        let quad = placement.quad(rect, colors, Cow::Borrowed(&picture.texture_coordinates));
        self.draw(placement, picture.material_index, &[quad], None);
    }

    /// NW4R window panes: the content quad shrunk by the frame and grown by its
    /// inflation, then one, four, or eight frame materials around it.
    fn window(
        &mut self,
        placement: &Placement<'_>,
        [x, y, width, height]: [f32; 4],
        window: &Window,
        colors: [[f32; 4]; 4],
    ) {
        let frames = &window.frames;
        let size = |index: usize| {
            frames.get(index).map_or([0.0; 2], |frame| {
                placement.texture_size(frame.material_index)
            })
        };
        let [left, right, top, bottom] = match frames.len() {
            1 => {
                let [width, height] = size(0);
                [width, width, height, height]
            }
            4 => [size(0)[0], size(3)[0], size(0)[1], size(3)[1]],
            8 => [size(4)[0], size(5)[0], size(6)[1], size(7)[1]],
            _ => [0.0; 4],
        };
        let [inflate_left, inflate_right, inflate_top, inflate_bottom] = window.inflation;
        let content = [
            x + left - inflate_left,
            y + top - inflate_top,
            width - left - right + inflate_left + inflate_right,
            height - top - bottom + inflate_top + inflate_bottom,
        ];
        let quad = placement.quad(
            content,
            colors,
            Cow::Borrowed(&window.content.texture_coordinates),
        );
        self.draw(placement, window.content.material_index, &[quad], None);
        let piece =
            |frame: usize, rect: [f32; 4], anchor: usize, stretch: bool, flip: Option<u8>| {
                FramePiece {
                    frame,
                    rect,
                    anchor,
                    stretch,
                    flip: flip.unwrap_or_else(|| frames.get(frame).map_or(0, |frame| frame.flip)),
                }
            };
        let (right_x, bottom_y) = (x + width - right, y + height - bottom);
        let pieces = match frames.len() {
            1 => vec![
                piece(0, [x, y, width - right, top], 0, true, Some(0)),
                piece(0, [right_x, y, right, height - bottom], 1, true, Some(1)),
                piece(
                    0,
                    [x + left, bottom_y, width - left, bottom],
                    3,
                    true,
                    Some(4),
                ),
                piece(0, [x, y + top, left, height - top], 2, true, Some(2)),
            ],
            4 => vec![
                piece(0, [x, y, width - right, top], 0, true, None),
                piece(1, [right_x, y, right, height - bottom], 1, true, None),
                piece(3, [x + left, bottom_y, width - left, bottom], 3, true, None),
                piece(2, [x, y + top, left, height - top], 2, true, None),
            ],
            8 => vec![
                piece(0, [x, y, left, top], 0, false, None),
                piece(1, [right_x, y, right, top], 0, false, None),
                piece(2, [x, bottom_y, left, bottom], 0, false, None),
                piece(3, [right_x, bottom_y, right, bottom], 0, false, None),
                piece(4, [x, y + top, left, height - top - bottom], 0, false, None),
                piece(
                    5,
                    [right_x, y + top, right, height - top - bottom],
                    0,
                    false,
                    None,
                ),
                piece(6, [x + left, y, width - left - right, top], 0, false, None),
                piece(
                    7,
                    [x + left, bottom_y, width - left - right, bottom],
                    0,
                    false,
                    None,
                ),
            ],
            _ => Vec::new(),
        };
        for piece in pieces {
            let Some(frame) = frames.get(piece.frame) else {
                continue;
            };
            let coordinates = vec![piece.coordinates(placement.texture_size(frame.material_index))];
            let quad = placement.quad(piece.rect, Self::WHITE, Cow::Owned(coordinates));
            self.draw(placement, frame.material_index, &[quad], None);
        }
    }

    /// Glyph quads from the pane's font, one draw per glyph sheet, colored top
    /// to bottom per glyph.
    fn text(
        &mut self,
        placement: &Placement<'_>,
        [x, y, width, height]: [f32; 4],
        text: &TextBox,
        colors: [[f32; 4]; 4],
    ) {
        let banner = placement.banner;
        let Some(name) = banner.layout.fonts.get(usize::from(text.font_index)) else {
            return;
        };
        let name = name.to_ascii_lowercase();
        let Some(font) = banner.fonts.get(&name) else {
            return;
        };
        let sheet = [
            f32::from(font.glyphs.width).max(1.0),
            f32::from(font.glyphs.height).max(1.0),
        ];
        let colors = [colors[0], colors[0], colors[2], colors[2]];
        let mut sheets = BTreeMap::<usize, Vec<Quad<'_>>>::new();
        for glyph in font.layout(text, [width, height]) {
            let [left, top, glyph_width, glyph_height] = glyph.source.map(f32::from);
            let [u0, v0] = [left / sheet[0], top / sheet[1]];
            let [u1, v1] = [
                (left + glyph_width) / sheet[0],
                (top + glyph_height) / sheet[1],
            ];
            let [dx, dy, dw, dh] = glyph.destination;
            let coordinates = vec![[[u0, v0], [u1, v0], [u0, v1], [u1, v1]]];
            sheets.entry(glyph.sheet).or_default().push(placement.quad(
                [x + dx, y + dy, dw, dh],
                colors,
                Cow::Owned(coordinates),
            ));
        }
        for (sheet, quads) in sheets {
            let texture = placement
                .textures
                .get(&WiiTarget::font_sheet(&name, sheet))
                .copied();
            self.draw(
                placement,
                text.material_index,
                &quads,
                Some(Slot {
                    texture,
                    wrap: [0; 2],
                }),
            );
        }
    }

    /// Draw `quads` with a material; `font` binds a glyph sheet to the first
    /// texture slot.
    fn draw(
        &mut self,
        placement: &Placement<'_>,
        material: u16,
        quads: &[Quad<'_>],
        font: Option<Slot>,
    ) {
        let banner = placement.banner;
        let Some(material) = banner.layout.materials.get(usize::from(material)) else {
            return;
        };
        let material_state = banner.timeline.material(material, placement.frame);
        let first = self.vertices.len() as u32;
        for quad in quads {
            self.vertices(material, &material_state, quad);
        }
        let mut slots = [Slot {
            texture: None,
            wrap: [0; 2],
        }; TEXTURE_SLOTS];
        for (index, reference) in material
            .texture_references
            .iter()
            .take(TEXTURE_SLOTS)
            .enumerate()
        {
            let name = material_state.textures[index]
                .and_then(|pattern| banner.timeline.textures().get(pattern))
                .or_else(|| {
                    banner
                        .layout
                        .textures
                        .get(usize::from(reference.texture_index))
                });
            slots[index] = Slot {
                texture: name
                    .and_then(|name| placement.textures.get(&name.to_ascii_lowercase()).copied()),
                wrap: [reference.wrap_s, reference.wrap_t],
            };
        }
        if let Some(font) = font {
            slots[0] = font;
        }
        self.materials.push(MaterialUniform::new(
            material,
            &material_state,
            font.is_some(),
        ));
        self.draws.push(Draw {
            blend: Blend::new(material),
            slots,
            vertices: first..self.vertices.len() as u32,
        });
    }

    fn vertices(&mut self, material: &Material, material_state: &MaterialState, quad: &Quad<'_>) {
        let coordinates = &quad.coordinates;
        let raster = |corner: usize| {
            let control = material.channel_control;
            let vertex = quad.colors[corner];
            let material_color = material_state.material_color;
            let color = if control.is_some_and(|control| control.color_material_source == 0) {
                material_color
            } else {
                vertex
            };
            let color_alpha = if control.is_some_and(|control| control.alpha_material_source == 0) {
                material_color[3]
            } else {
                vertex[3]
            };
            [
                color[0] / 255.0,
                color[1] / 255.0,
                color[2] / 255.0,
                color_alpha / 255.0 * quad.alpha,
            ]
        };
        let texture_coordinate = |corner: usize, generator: usize| {
            let Some(generator) = material.texture_coordinate_generators.get(generator) else {
                return coordinates.first().map_or([0.0; 2], |set| set[corner]);
            };
            let source = usize::from(generator.source.saturating_sub(TEXTURE_COORDINATE_BASE));
            let uv = coordinates
                .get(source)
                .or(coordinates.first())
                .map_or([0.0; 2], |set| set[corner]);
            let matrix = generator
                .matrix_source
                .checked_sub(TEXTURE_MATRIX_BASE)
                .map(|offset| usize::from(offset / 3));
            matrix
                .and_then(|index| material_state.texture_srts.get(index))
                .map_or(uv, |srt: &TextureSrt| srt.transform(uv))
        };
        for corner in [0, 1, 2, 2, 1, 3] {
            self.vertices.push(Vertex {
                position: quad.corners[corner],
                color: raster(corner),
                uv: std::array::from_fn(|generator| texture_coordinate(corner, generator)),
            });
        }
    }
}

pub(super) struct WiiRenderer {
    shader: wgpu::ShaderModule,
    material_layout: wgpu::BindGroupLayout,
    texture_layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    pipelines: HashMap<Blend, wgpu::RenderPipeline>,
    samplers: HashMap<[u8; 2], wgpu::Sampler>,
    blank: wgpu::TextureView,
}

impl WiiRenderer {
    pub(super) fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("polymer wii banner shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("wii.wgsl").into()),
        });
        let material_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("polymer wii banner material"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(size_of::<MaterialUniform>() as u64),
                },
                count: None,
            }],
        });
        let textures = (0..TEXTURE_SLOTS as u32).map(|binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        });
        let samplers = (0..TEXTURE_SLOTS as u32).map(|slot| wgpu::BindGroupLayoutEntry {
            binding: TEXTURE_SLOTS as u32 + slot,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("polymer wii banner textures"),
            entries: &textures.chain(samplers).collect::<Vec<_>>(),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("polymer wii banner layout"),
            bind_group_layouts: &[Some(&material_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let blank = Self::upload(device, queue, "polymer wii banner blank", 1, 1, &[255; 4]);
        Self {
            shader,
            material_layout,
            texture_layout,
            pipeline_layout,
            pipelines: HashMap::new(),
            samplers: HashMap::new(),
            blank,
        }
    }

    fn upload(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &str,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> wgpu::TextureView {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            texture.as_image_copy(),
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: None,
            },
            texture.size(),
        );
        texture.create_view(&Default::default())
    }

    fn render_texture(
        device: &wgpu::Device,
        label: &str,
        width: u32,
        height: u32,
        samples: u32,
        usage: wgpu::TextureUsages,
    ) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: samples,
            dimension: wgpu::TextureDimension::D2,
            format: COLOR_FORMAT,
            usage,
            view_formats: &[],
        })
    }

    fn materials(&self, device: &wgpu::Device, size: u64) -> (wgpu::Buffer, wgpu::BindGroup) {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("polymer wii banner materials"),
            size,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("polymer wii banner materials"),
            layout: &self.material_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &buffer,
                    offset: 0,
                    size: wgpu::BufferSize::new(size_of::<MaterialUniform>() as u64),
                }),
            }],
        });
        (buffer, bindings)
    }

    fn vertex_buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("polymer wii banner vertices"),
            size,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    fn pipeline(&mut self, device: &wgpu::Device, blend: Blend) {
        if self.pipelines.contains_key(&blend) {
            return;
        }
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("polymer wii banner pipeline"),
            layout: Some(&self.pipeline_layout),
            vertex: wgpu::VertexState {
                module: &self.shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4, 4 => Float32x4, 5 => Float32x4],
                }],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: SAMPLES,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &self.shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: COLOR_FORMAT,
                    blend: Some(blend.state()),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        self.pipelines.insert(blend, pipeline);
    }

    /// GX wrap modes: clamp, repeat, and mirror.
    fn sampler(&mut self, device: &wgpu::Device, wrap: [u8; 2]) -> wgpu::Sampler {
        self.samplers
            .entry(wrap)
            .or_insert_with(|| {
                let mode = |wrap: u8| match wrap {
                    1 => wgpu::AddressMode::Repeat,
                    2 => wgpu::AddressMode::MirrorRepeat,
                    _ => wgpu::AddressMode::ClampToEdge,
                };
                device.create_sampler(&wgpu::SamplerDescriptor {
                    label: Some("polymer wii banner sampler"),
                    address_mode_u: mode(wrap[0]),
                    address_mode_v: mode(wrap[1]),
                    mag_filter: wgpu::FilterMode::Linear,
                    min_filter: wgpu::FilterMode::Linear,
                    ..Default::default()
                })
            })
            .clone()
    }

    fn bindings(
        &mut self,
        device: &wgpu::Device,
        target: &mut WiiTarget,
        slots: [Slot; TEXTURE_SLOTS],
    ) {
        if target.bindings.contains_key(&slots) {
            return;
        }
        let samplers = slots.map(|slot| self.sampler(device, slot.wrap));
        let views = slots.map(|slot| {
            slot.texture
                .and_then(|texture| target.views.get(texture))
                .unwrap_or(&self.blank)
                .clone()
        });
        let entries = views
            .iter()
            .enumerate()
            .map(|(binding, view)| wgpu::BindGroupEntry {
                binding: binding as u32,
                resource: wgpu::BindingResource::TextureView(view),
            })
            .chain(
                samplers
                    .iter()
                    .enumerate()
                    .map(|(slot, sampler)| wgpu::BindGroupEntry {
                        binding: (TEXTURE_SLOTS + slot) as u32,
                        resource: wgpu::BindingResource::Sampler(sampler),
                    }),
            )
            .collect::<Vec<_>>();
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("polymer wii banner textures"),
            layout: &self.texture_layout,
            entries: &entries,
        });
        target.bindings.insert(slots, group);
    }

    pub(super) fn target(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        banner: Arc<WiiBanner>,
        width: u32,
        height: u32,
    ) -> WiiTarget {
        let mut names = HashMap::new();
        let mut views = Vec::new();
        for (name, image) in &banner.textures {
            names.insert(name.clone(), views.len());
            views.push(Self::upload(
                device,
                queue,
                "polymer wii banner texture",
                u32::from(image.width).max(1),
                u32::from(image.height).max(1),
                &image.rgba,
            ));
        }
        for (name, font) in &banner.fonts {
            for (index, sheet) in font.glyphs.sheets.iter().enumerate() {
                names.insert(WiiTarget::font_sheet(name, index), views.len());
                views.push(Self::upload(
                    device,
                    queue,
                    "polymer wii banner glyphs",
                    u32::from(font.glyphs.width).max(1),
                    u32::from(font.glyphs.height).max(1),
                    &sheet.rgba,
                ));
            }
        }
        let (material_buffer, material_bindings) = self.materials(device, MATERIAL_STRIDE);
        let vertex_buffer = Self::vertex_buffer(device, 6 * size_of::<Vertex>() as u64);
        let output = Self::render_texture(
            device,
            "polymer wii banner output",
            width,
            height,
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let multisampled = Self::render_texture(
            device,
            "polymer wii banner multisample",
            width,
            height,
            SAMPLES,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        WiiTarget {
            banner,
            names,
            views,
            bindings: HashMap::new(),
            material_buffer,
            material_bindings,
            vertex_buffer,
            output_view: output.create_view(&Default::default()),
            multisampled_view: multisampled.create_view(&Default::default()),
            output,
            started: Instant::now(),
        }
    }

    pub(super) fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &mut WiiTarget,
    ) {
        let elapsed = target.started.elapsed().as_secs_f32() * FRAMES_PER_SECOND;
        let frame = Frame::build(
            &target.banner,
            &target.names,
            target.banner.timeline.frame(elapsed),
        );
        for draw in &frame.draws {
            self.pipeline(device, draw.blend);
            self.bindings(device, target, draw.slots);
        }
        let material_size =
            (frame.materials.len().max(1) as u64 * MATERIAL_STRIDE).next_power_of_two();
        if target.material_buffer.size() < material_size {
            (target.material_buffer, target.material_bindings) =
                self.materials(device, material_size);
        }
        let vertex_size = (frame.vertices.len().max(1) * size_of::<Vertex>()) as u64;
        if target.vertex_buffer.size() < vertex_size {
            target.vertex_buffer = Self::vertex_buffer(device, vertex_size.next_power_of_two());
        }
        let stride = MATERIAL_STRIDE as usize;
        let mut materials = vec![0; frame.materials.len() * stride];
        for (index, material) in frame.materials.iter().enumerate() {
            let start = index * stride;
            materials[start..start + size_of::<MaterialUniform>()]
                .copy_from_slice(bytemuck::bytes_of(material));
        }
        queue.write_buffer(&target.material_buffer, 0, &materials);
        queue.write_buffer(
            &target.vertex_buffer,
            0,
            bytemuck::cast_slice(&frame.vertices),
        );
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("polymer wii banner"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("polymer wii banner"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.multisampled_view,
                    depth_slice: None,
                    resolve_target: Some(&target.output_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Discard,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_vertex_buffer(0, target.vertex_buffer.slice(..));
            for (index, draw) in frame.draws.iter().enumerate() {
                pass.set_pipeline(&self.pipelines[&draw.blend]);
                pass.set_bind_group(0, &target.material_bindings, &[(index * stride) as u32]);
                pass.set_bind_group(1, &target.bindings[&draw.slots], &[]);
                pass.draw(draw.vertices.clone(), 0..1);
            }
        }
        queue.submit([encoder.finish()]);
    }
}

/// GPU resources for one Wii banner placeholder, redrawn in place every frame.
pub(super) struct WiiTarget {
    banner: Arc<WiiBanner>,
    names: HashMap<String, usize>,
    views: Vec<wgpu::TextureView>,
    bindings: HashMap<[Slot; TEXTURE_SLOTS], wgpu::BindGroup>,
    material_buffer: wgpu::Buffer,
    material_bindings: wgpu::BindGroup,
    vertex_buffer: wgpu::Buffer,
    pub(super) output: wgpu::Texture,
    output_view: wgpu::TextureView,
    multisampled_view: wgpu::TextureView,
    started: Instant,
}

impl WiiTarget {
    fn font_sheet(font: &str, sheet: usize) -> String {
        format!("{font}#{sheet}")
    }
}
