//! Splits a painted document into chrome bands and game-view placements.
//!
//! Blitz lays the game layer out, but guest frames must not go through a 2D
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

use anyrender::{CustomPaint, Glyph, NormalizedCoord, Paint, PaintRef, PaintScene};
use kurbo::{Affine, BezPath, Rect, Shape, Stroke};
use peniko::{BlendMode, Color, Fill, FontData, StyleRef};

/// Curve tolerance for flattening a clip shape so it can be replayed. Well
/// below a pixel, so a replayed clip is indistinguishable from the original.
const CLIP_TOLERANCE: f64 = 0.05;

/// Where one game view landed, and which chrome band it is drawn over.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GamePlacement {
    /// The `src` of the `<tracked-surface>`, naming which tracked render target
    /// belongs in this box.
    pub source_id: u64,
    pub rect: Rect,
    /// Index of the band painted immediately below this view. The view is
    /// composited above that band and below the next one.
    pub band: usize,
}

/// A clip layer left open across a band switch.
struct OpenLayer {
    blend: BlendMode,
    alpha: f32,
    transform: Affine,
    clip: BezPath,
}

/// Routes a document's paint into a series of chrome bands, diverting
/// game-view fills into [`GamePlacement`]s.
pub struct BandedScene<'a, S> {
    bands: &'a mut [S],
    placements: &'a mut Vec<GamePlacement>,
    current: usize,
    stack: Vec<OpenLayer>,
    overflowed: bool,
}

impl<'a, S: PaintScene> BandedScene<'a, S> {
    /// `bands` must hold one more scene than the document has game views.
    pub fn new(bands: &'a mut [S], placements: &'a mut Vec<GamePlacement>) -> Self {
        Self {
            bands,
            placements,
            current: 0,
            stack: Vec::new(),
            overflowed: false,
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

    /// Closes the open layer stack on the band being left and reopens it on the
    /// next one, so clipping carries across the seam.
    fn advance_band(&mut self) {
        if self.current + 1 >= self.bands.len() {
            self.overflowed = true;
            return;
        }
        for _ in 0..self.stack.len() {
            self.bands[self.current].pop_layer();
        }
        self.current += 1;
        let (bands, stack) = (&mut *self.bands, &self.stack);
        for layer in stack {
            bands[self.current].push_layer(layer.blend, layer.alpha, layer.transform, &layer.clip);
        }
    }
}

impl<S: PaintScene> PaintScene for BandedScene<'_, S> {
    fn reset(&mut self) {
        self.placements.clear();
        self.stack.clear();
        self.current = 0;
        self.overflowed = false;
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
    ) {
        let blend = blend.into();
        let clip = clip.into_path(CLIP_TOLERANCE);
        self.band().push_layer(blend, alpha, transform, &clip);
        self.stack.push(OpenLayer {
            blend,
            alpha,
            transform,
            clip,
        });
    }

    fn pop_layer(&mut self) {
        self.stack.pop();
        self.band().pop_layer();
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
        if let Paint::Custom(custom) = brush
            && let Some(custom) = custom.downcast_ref::<CustomPaint>()
        {
            // The guest frame belongs here. Leave the pixels untouched so the
            // compositor can draw the texture into the hole, and start a new
            // band so anything painted afterwards lands above it.
            self.placements.push(GamePlacement {
                source_id: custom.source_id,
                rect: transform.transform_rect_bbox(shape.bounding_box()),
                band: self.current,
            });
            self.advance_band();
            return;
        }
        self.band()
            .fill(style, transform, brush, brush_transform, shape);
    }

    fn draw_glyphs<'a, 'b: 'a>(
        &'b mut self,
        font: &'a FontData,
        font_size: f32,
        hint: bool,
        normalized_coords: &'a [NormalizedCoord],
        style: impl Into<StyleRef<'a>>,
        brush: impl Into<PaintRef<'a>>,
        brush_alpha: f32,
        transform: Affine,
        glyph_transform: Option<Affine>,
        glyphs: impl Iterator<Item = Glyph>,
    ) {
        self.bands[self.current].draw_glyphs(
            font,
            font_size,
            hint,
            normalized_coords,
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
    }
}
