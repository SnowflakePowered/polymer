use kurbo::Rect;
use polymer_renderer::SurfaceContent;

/// A complete shell raster/visual publication. Layout metadata stays with its caller.
pub struct CompositionScene {
    pub revision: u64,
    pub raster_chunks: Vec<ShellRasterChunk>,
    pub composition_groups: Vec<ShellCompositionGroup>,
    pub width: u32,
    pub height: u32,
    /// Image mip chains from earlier generations are evicted.
    pub mip_generation: u64,
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompositionShadowStyle {
    pub offset: [f32; 2],
    pub blur_radius: f32,
    pub color: [u8; 4],
}
/// Logical captured content; its native resource is owned by the backend.
pub struct SurfaceLayer {
    pub connection: u64,
    pub instance: u32,
    pub parent: Option<u64>,
    pub z_order: i32,
    pub placement: SurfaceContent,
}
/// Captured streams stitched into one group swapchain, in group-local pixels.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SurfaceGroupLayout {
    pub extent: polymer_renderer::Extent,
    pub members: Vec<SurfaceGroupMember>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceGroupMember {
    pub connection: u64,
    pub rectangle: polymer_renderer::layout::Rect,
    pub corner_radius: f32,
    pub placement: SurfaceContent,
}

#[derive(Clone)]
pub struct ShellRasterChunk {
    pub id: RasterChunkId,
    pub scene: anyrender::Scene,
    pub bounds: Rect,
    pub z_order: i32,
    pub group: Option<u64>,
}

/// Stable identity for one raster leaf within a retained compositor owner.
/// Root chrome and each promoted stacking context have independent ordinal
/// spaces, so unrelated group insertion does not churn existing WUC visuals.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RasterChunkId {
    pub owner: Option<u64>,
    pub ordinal: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RasterChunkLayout {
    pub id: RasterChunkId,
    pub bounds: Rect,
    pub z_order: i32,
    pub group: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShellCompositionGroup {
    pub id: u64,
    pub parent: Option<u64>,
    pub bounds: Rect,
    pub z_order: i32,
    pub opacity: f32,
    pub shadow: Option<CompositionShadowStyle>,
}
