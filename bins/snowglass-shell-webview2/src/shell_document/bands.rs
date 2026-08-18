//! Splits a painted document into chrome bands and game-view placements.
//!
//! Blitz lays the shell document out, but guest frames must not go through a 2D
//! renderer: they are already GPU textures and the compositor samples them
//! directly. So the paint pass forwards everything to real scenes except the
//! nodes standing in for game views, which are recorded as rectangles instead.
//!
//! A game view is written as `<tracked-surface src="N">`, which Snowglass
//! exposes to Blitz as a
//! [`CustomPaint`] carrying the source id and the laid-out box. That is the
//! same escape hatch a GPU backend uses to place externally-rendered content in
//! a document, and it is fail-safe here: a backend that does not recognise a
//! custom paint draws nothing rather than something wrong.
//!
//! One chrome image would not be enough: a game view can have chrome both under
//! and over it, as the handheld bezel does when it sits on top of a fullscreen
//! view. So each view starts a new *band*, and the compositor stacks
//! band, view, band, view, … to reproduce the document's paint order. Bands are
//! separate scenes, so the layer stack open at a switch is closed on the band
//! being left and reopened on the band being entered — otherwise a clip
//! established before a game view would stop applying after it.

use anyrender::{
    Filter, Glyph, NormalizedCoord, Paint, PaintRef, PaintScene, RenderContext,
    recording::RenderCommand,
};
use kurbo::{Affine, BezPath, Rect, Shape, Stroke, Vec2};
use peniko::{BlendMode, Color, Fill, FontData, StyleRef};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use style::{
    properties::generated::longhands::object_fit::computed_value::T as ObjectFit,
    values::computed::position::Position,
};

/// Curve tolerance for flattening a clip shape so it can be replayed. Well
/// below a pixel, so a replayed clip is indistinguishable from the original.
const CLIP_TOLERANCE: f64 = 0.05;

/// Computed CSS properties the GPU compositor needs for a game view.
#[derive(Debug, Clone, PartialEq)]
pub struct GameViewStyle {
    pub object_fit: ObjectFit,
    pub object_position: Position,
}

/// An outset box shadow that can be applied by the retained native
/// compositor instead of being baked into a full-window chrome texture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompositionShadowStyle {
    pub offset: [f32; 2],
    pub blur_radius: f32,
    pub color: [u8; 4],
}

/// Where one game view landed, and which chrome band it is drawn over.
#[derive(Debug, Clone, PartialEq)]
pub struct GamePlacement {
    /// Blitz DOM node whose custom paint produced this placement.
    pub node_id: Option<usize>,
    /// The `src` of the `<tracked-surface>`, naming which tracked render target
    /// belongs in this box.
    pub source_id: u64,
    pub rect: Rect,
    /// Conservative physical-pixel clip accumulated from every open paint
    /// layer. Non-rectangular paths are represented by their transformed
    /// bounding rectangle because the native compositor consumes rectangles.
    pub clipping: Option<Rect>,
    /// Corner radius, in physical pixels, applied by the merged GPU
    /// compositor to the guest texture.
    pub corner_radius: f32,
    /// CSS image sizing and alignment resolved for this tracked surface.
    ///
    /// The painter only gives custom paints their box and source id, so this is
    /// populated from the DOM immediately after painting.
    pub style: Option<GameViewStyle>,
    /// Effective opacity of the open Blitz layer stack.
    pub opacity: f32,
    /// Index of the band painted immediately below this view. The view is
    /// composited above that band and below the next one.
    pub band: usize,
    /// Nearest retained paint layer shared with this view. Chrome fragments
    /// carrying the same id can move independently from unrelated content.
    pub group: Option<u64>,
}

pub struct BandFragment {
    pub band: usize,
    pub group: Option<u64>,
    pub shadow: Option<CompositionShadowStyle>,
    pub scene: anyrender::Scene,
}

#[derive(Clone, Copy)]
enum CommandMeta {
    Push(u64),
    Pop,
    Draw,
}

/// A clip layer left open across a band switch.
struct OpenLayer {
    id: u64,
    blend: Option<BlendMode>,
    alpha: f32,
    transform: Affine,
    clip: BezPath,
    filter: Option<Arc<Filter>>,
    backdrop_filter: Option<Arc<Filter>>,
}

/// Routes a document's paint into a series of chrome bands, diverting
/// game-view fills into [`GamePlacement`]s.
pub struct BandedScene<'a, S> {
    bands: &'a mut [S],
    placements: &'a mut Vec<GamePlacement>,
    current: usize,
    stack: Vec<OpenLayer>,
    overflowed: bool,
    next_layer_id: u64,
    command_meta: Vec<Vec<CommandMeta>>,
    promoted_layers: HashSet<u64>,
}

impl<'a, S: PaintScene> BandedScene<'a, S> {
    /// `bands` must hold one more scene than the document has game views.
    pub fn new(bands: &'a mut [S], placements: &'a mut Vec<GamePlacement>) -> Self {
        let band_count = bands.len();
        Self {
            bands,
            placements,
            current: 0,
            stack: Vec::new(),
            overflowed: false,
            next_layer_id: 1,
            command_meta: (0..band_count).map(|_| Vec::new()).collect(),
            promoted_layers: HashSet::new(),
        }
    }

    /// How many bands actually received content.
    pub fn bands_used(&self) -> usize {
        self.current + 1
    }

    /// Whether the document held more game views than there were bands for, in
    /// which case the views past the last band share one and cannot be stacked
    /// against the chrome between them.
    pub fn overflowed(&self) -> bool {
        self.overflowed
    }

    fn band(&mut self) -> &mut S {
        &mut self.bands[self.current]
    }

    fn record(&mut self, meta: CommandMeta) {
        self.command_meta[self.current].push(meta);
    }

    /// Closes the open layer stack on the band being left and reopens it on the
    /// next one, so clipping carries across the seam.
    fn advance_band(&mut self) {
        if self.current + 1 >= self.bands.len() {
            self.overflowed = true;
            return;
        }
        for _ in 0..self.stack.len() {
            self.bands[self.current].pop_layer();
            self.command_meta[self.current].push(CommandMeta::Pop);
        }
        self.current += 1;
        let (bands, stack) = (&mut *self.bands, &self.stack);
        for layer in stack {
            if let Some(blend) = layer.blend {
                bands[self.current].push_layer(
                    blend,
                    layer.alpha,
                    layer.transform,
                    &layer.clip,
                    layer.filter.clone(),
                    layer.backdrop_filter.clone(),
                );
            } else {
                bands[self.current].push_clip_layer(layer.transform, &layer.clip);
            }
            self.command_meta[self.current].push(CommandMeta::Push(layer.id));
        }
    }
}

impl BandedScene<'_, anyrender::Scene> {
    /// Splits each paint band into contiguous retained fragments. The deepest
    /// paint layer enclosing a tracked surface becomes an independent owner;
    /// unrelated chrome in the same z band remains in separate WUC visuals.
    pub fn take_fragments(&mut self) -> Vec<BandFragment> {
        struct Builder {
            group: Option<u64>,
            shadow: Option<CompositionShadowStyle>,
            scene: anyrender::Scene,
            stack: Vec<(u64, RenderCommand)>,
        }

        impl Builder {
            fn close(
                mut self,
                output: &mut Vec<(
                    Option<u64>,
                    Option<CompositionShadowStyle>,
                    anyrender::Scene,
                )>,
            ) {
                for _ in 0..self.stack.len() {
                    self.scene.commands.push(RenderCommand::PopLayer);
                }
                output.push((self.group, self.shadow, self.scene));
            }

            fn reconcile(&mut self, desired: &[(u64, RenderCommand)]) {
                let common = self
                    .stack
                    .iter()
                    .zip(desired)
                    .take_while(|((left, _), (right, _))| left == right)
                    .count();
                for _ in common..self.stack.len() {
                    self.scene.commands.push(RenderCommand::PopLayer);
                }
                self.stack.truncate(common);
                for entry in &desired[common..] {
                    self.scene.commands.push(entry.1.clone());
                    self.stack.push(entry.clone());
                }
            }
        }

        let mut fragments = Vec::new();
        for band in 0..self.bands_used() {
            let scene = &mut self.bands[band];
            let commands = std::mem::take(&mut scene.commands);
            let metadata = std::mem::take(&mut self.command_meta[band]);
            debug_assert_eq!(commands.len(), metadata.len());
            // Blitz paints an element's outset shadow immediately before the
            // clip layer that owns its background and descendants. Carry that
            // shadow with a promoted layer so WUC moves both together.
            let mut prelude_owner = HashMap::new();
            let mut native_shadows = HashMap::new();
            for (index, meta) in metadata.iter().enumerate() {
                let CommandMeta::Push(id) = meta else {
                    continue;
                };
                if self.promoted_layers.contains(id)
                    && let Some(shadow) = (0..index)
                        .rev()
                        .find(|candidate| matches!(metadata[*candidate], CommandMeta::Draw))
                    && matches!(commands[shadow], RenderCommand::BoxShadow(_))
                {
                    prelude_owner.insert(shadow, *id);
                    let RenderCommand::BoxShadow(shadow_command) = &commands[shadow] else {
                        unreachable!("the shadow command was checked above")
                    };
                    let layer_transform = match &commands[index] {
                        RenderCommand::PushLayer(layer) => layer.transform,
                        RenderCommand::PushClipLayer(layer) => layer.transform,
                        _ => unreachable!("push metadata must describe a push command"),
                    };
                    let shadow_transform = shadow_command.transform.as_coeffs();
                    let layer_transform = layer_transform.as_coeffs();
                    let color = shadow_command.brush.to_rgba8().to_u8_array();
                    native_shadows.insert(
                        *id,
                        CompositionShadowStyle {
                            offset: [
                                (shadow_transform[4] - layer_transform[4]) as f32,
                                (shadow_transform[5] - layer_transform[5]) as f32,
                            ],
                            blur_radius: shadow_command.std_dev.max(0.0) as f32,
                            color,
                        },
                    );
                }
            }
            let mut source_stack = Vec::<(u64, RenderCommand)>::new();
            let mut runs = Vec::new();
            let mut builder = None::<Builder>;
            for (index, (command, meta)) in commands.into_iter().zip(metadata).enumerate() {
                match meta {
                    CommandMeta::Push(id) => source_stack.push((id, command)),
                    CommandMeta::Pop => {
                        source_stack.pop();
                    }
                    CommandMeta::Draw => {
                        let group = prelude_owner.get(&index).copied().or_else(|| {
                            source_stack.iter().rev().find_map(|(id, _)| {
                                self.promoted_layers.contains(id).then_some(*id)
                            })
                        });
                        if builder
                            .as_ref()
                            .is_some_and(|builder| builder.group != group)
                            && let Some(builder) = builder.take()
                        {
                            builder.close(&mut runs);
                        }
                        let builder = builder.get_or_insert_with(|| Builder {
                            group,
                            shadow: group.and_then(|group| native_shadows.get(&group).copied()),
                            scene: anyrender::Scene::with_tolerance(scene.tolerance),
                            stack: Vec::new(),
                        });
                        builder.reconcile(&source_stack);
                        // A compositor-owned shadow follows the retained visual
                        // through translation without repainting or leaving old
                        // blurred pixels in the swapchain.
                        if !native_shadows
                            .get(&group.unwrap_or_default())
                            .is_some_and(|_| prelude_owner.contains_key(&index))
                        {
                            builder.scene.commands.push(command);
                        }
                    }
                }
            }
            if let Some(builder) = builder {
                builder.close(&mut runs);
            }
            fragments.extend(runs.into_iter().map(|(group, shadow, scene)| BandFragment {
                band,
                group,
                shadow,
                scene,
            }));
        }
        fragments
    }
}

impl<S> RenderContext for BandedScene<'_, S> {}

impl<S: PaintScene> PaintScene for BandedScene<'_, S> {
    fn reset(&mut self) {
        self.placements.clear();
        self.stack.clear();
        self.current = 0;
        self.overflowed = false;
        self.next_layer_id = 1;
        self.promoted_layers.clear();
        for metadata in &mut self.command_meta {
            metadata.clear();
        }
        for band in self.bands.iter_mut() {
            band.reset();
        }
    }

    fn push_layer(
        &mut self,
        blend: impl Into<BlendMode>,
        alpha: f32,
        transform: Affine,
        clip: &impl Shape,
        filter: Option<Arc<Filter>>,
        backdrop_filter: Option<Arc<Filter>>,
    ) {
        let blend = blend.into();
        let clip = clip.into_path(CLIP_TOLERANCE);
        let id = self.next_layer_id;
        self.next_layer_id += 1;
        self.band().push_layer(
            blend,
            alpha,
            transform,
            &clip,
            filter.clone(),
            backdrop_filter.clone(),
        );
        self.stack.push(OpenLayer {
            id,
            blend: Some(blend),
            alpha,
            transform,
            clip,
            filter,
            backdrop_filter,
        });
        self.record(CommandMeta::Push(id));
    }

    fn push_clip_layer(&mut self, transform: Affine, clip: &impl Shape) {
        let clip = clip.into_path(CLIP_TOLERANCE);
        let id = self.next_layer_id;
        self.next_layer_id += 1;
        self.band().push_clip_layer(transform, &clip);
        self.stack.push(OpenLayer {
            id,
            blend: None,
            alpha: 1.0,
            transform,
            clip,
            filter: None,
            backdrop_filter: None,
        });
        self.record(CommandMeta::Push(id));
    }

    fn pop_layer(&mut self) {
        self.stack.pop();
        self.band().pop_layer();
        self.record(CommandMeta::Pop);
    }

    fn stroke<'a>(
        &mut self,
        style: &Stroke,
        transform: Affine,
        brush: impl Into<PaintRef<'a>>,
        brush_transform: Option<Affine>,
        shape: &impl Shape,
    ) {
        self.band()
            .stroke(style, transform, brush, brush_transform, shape);
        self.record(CommandMeta::Draw);
    }

    fn fill<'a>(
        &mut self,
        style: Fill,
        transform: Affine,
        brush: impl Into<PaintRef<'a>>,
        brush_transform: Option<Affine>,
        shape: &impl Shape,
    ) {
        let brush = brush.into();
        if let Paint::Resource(resource) = brush {
            // The guest frame belongs here. Leave the pixels untouched so the
            // compositor can draw the texture into the hole, and start a new
            // band so anything painted afterwards lands above it.
            let clipping = self
                .stack
                .iter()
                .map(|layer| {
                    layer
                        .transform
                        .transform_rect_bbox(layer.clip.bounding_box())
                })
                .reduce(|current, next| current.intersect(next));
            self.placements.push(GamePlacement {
                node_id: None,
                source_id: resource.image.into_ffi(),
                rect: transform.transform_rect_bbox(shape.bounding_box()),
                clipping,
                corner_radius: 0.0,
                style: None,
                opacity: self.stack.iter().map(|layer| layer.alpha).product(),
                band: self.current,
                group: self.stack.last().map(|layer| layer.id),
            });
            if let Some(layer) = self.stack.last() {
                self.promoted_layers.insert(layer.id);
            }
            self.advance_band();
            return;
        }
        self.band()
            .fill(style, transform, brush, brush_transform, shape);
        self.record(CommandMeta::Draw);
    }

    fn draw_glyphs<'a, 'b: 'a>(
        &'b mut self,
        font: &'a FontData,
        font_size: f32,
        hint: bool,
        normalized_coords: &'a [NormalizedCoord],
        embolden: Vec2,
        style: impl Into<StyleRef<'a>>,
        brush: impl Into<PaintRef<'a>>,
        brush_alpha: f32,
        transform: Affine,
        glyph_transform: Option<Affine>,
        glyphs: impl Iterator<Item = Glyph> + Clone,
    ) {
        self.record(CommandMeta::Draw);
        self.bands[self.current].draw_glyphs(
            font,
            font_size,
            hint,
            normalized_coords,
            embolden,
            style,
            brush,
            brush_alpha,
            transform,
            glyph_transform,
            glyphs,
        );
    }

    fn draw_box_shadow(
        &mut self,
        transform: Affine,
        rect: Rect,
        brush: Color,
        radius: f64,
        std_dev: f64,
    ) {
        self.band()
            .draw_box_shadow(transform, rect, brush, radius, std_dev);
        self.record(CommandMeta::Draw);
    }
}
