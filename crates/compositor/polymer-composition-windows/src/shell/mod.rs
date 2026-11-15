//! Retained shell raster and WUC composition ownership.

mod backdrop;
mod chrome;
mod chunk;
mod raster;

pub(crate) use raster::ShellRasterizer;

use crate::surfaces::RenderDevice;
use backdrop::BackdropBlurs;
use chunk::{ChunkSurface, ChunkSurfaces};
use polymer_composition::{
    CompositionScene as ShellFrame, CompositionShadowStyle, RasterChunkId, RasterChunkLayout,
    ShellCompositionGroup, SurfaceLayer,
};
use polymer_protocol::AdapterLuid;
use raster::{ShellRasterResult, ShellRasterWorker};
use std::{
    collections::{HashMap, HashSet},
    error::Error,
    sync::Arc,
};
use windows::{
    UI::Composition::{
        CompositionDropShadowSourcePolicy, CompositionShadow, CompositionStretch,
        CompositionSurfaceBrush, Compositor, ContainerVisual, DropShadow, InsetClip, LayerVisual,
        SpriteVisual, Visual,
    },
    Win32::System::WinRT::Composition::ICompositorInterop,
    core::{IUnknown, Interface},
};
use windows_numerics::{Vector2, Vector3};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum CompositionLayerId {
    Raster(RasterChunkId),
    Surface(u64, u32),
}

#[derive(Clone, Copy)]
enum CompositionLayerStretch {
    None,
    Fill,
    UniformToFill,
}

enum CompositionLayerContent {
    Swapchain(IUnknown),
    Surface(windows::UI::Composition::ICompositionSurface),
}

impl CompositionLayerContent {
    fn identity(&self) -> usize {
        match self {
            Self::Swapchain(content) => content.as_raw() as usize,
            Self::Surface(content) => content.as_raw() as usize,
        }
    }
}

struct CompositionLayer {
    id: CompositionLayerId,
    parent: Option<u64>,
    z_order: i32,
    content: CompositionLayerContent,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    opacity: f32,
    stretch: CompositionLayerStretch,
    horizontal_alignment: f32,
    vertical_alignment: f32,
}

struct CompositionGroup {
    id: u64,
    parent: Option<u64>,
    z_order: i32,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    opacity: f32,
    shadow: Option<CompositionShadowStyle>,
    backdrop_blur: Option<f32>,
}

struct CompositionContents {
    groups: Vec<CompositionGroup>,
    layers: Vec<CompositionLayer>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum CompositionNodeId {
    Backdrop(u64),
    Group(u64),
    Layer(CompositionLayerId),
}

struct CompositionVisual {
    visual: SpriteVisual,
    brush: CompositionSurfaceBrush,
    content_identity: usize,
}

struct CompositionGroupVisual {
    visual: LayerVisual,
    clip: InsetClip,
    shadow: Option<DropShadow>,
}

struct ShellDeviceContext {
    adapter: AdapterLuid,
    device: IUnknown,
    queue: IUnknown,
}

struct RasterDomain {
    context: ShellDeviceContext,
    worker: ShellRasterWorker,
    chunk_surfaces: ChunkSurfaces,
    surfaces: HashMap<RasterChunkId, ChunkSurface>,
    chunks: Vec<RasterChunkLayout>,
    groups: Vec<ShellCompositionGroup>,
    /// A finished raster result waiting for the reconcile that commits it.
    pending: Option<ShellRasterResult>,
}

impl RasterDomain {
    fn new(
        compositor: &Compositor,
        rasterizer: ShellRasterizer,
        publication_ready: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, Box<dyn Error>> {
        let context = rasterizer.device_context()?;
        Ok(Self {
            chunk_surfaces: ChunkSurfaces::new(compositor, &context)?,
            context,
            worker: ShellRasterWorker::spawn(rasterizer, publication_ready),
            surfaces: HashMap::new(),
            chunks: Vec::new(),
            groups: Vec::new(),
            pending: None,
        })
    }
}

/// Owns every shell-specific GPU and retained-composition resource. GameHost
/// supplies captured-surface snapshots but never translates the shell tree.
pub(crate) struct ShellCompositor {
    compositor: Compositor,
    root: ContainerVisual,
    visuals: HashMap<CompositionLayerId, CompositionVisual>,
    swapchain_surfaces: HashMap<usize, (IUnknown, windows::UI::Composition::ICompositionSurface)>,
    group_visuals: HashMap<u64, CompositionGroupVisual>,
    backdrops: BackdropBlurs,
    child_orders: HashMap<Option<u64>, Vec<CompositionNodeId>>,
    raster: RasterDomain,
    publication_ready: Arc<dyn Fn() + Send + Sync>,
    content_dirty: bool,
    /// Chunk pixels were copied into drawing surfaces, which only appear with
    /// a composition commit even when the layout is unchanged.
    pixels_dirty: bool,
    game_revision: u64,
}

impl ShellCompositor {
    pub(crate) fn new(
        compositor: &Compositor,
        rasterizer: ShellRasterizer,
        publication_ready: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, Box<dyn Error>> {
        let publication_ready: Arc<dyn Fn() + Send + Sync> = Arc::new(publication_ready);
        let raster = RasterDomain::new(compositor, rasterizer, publication_ready.clone())?;
        let root = compositor.CreateContainerVisual()?;
        root.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;
        Ok(Self {
            compositor: compositor.clone(),
            root,
            visuals: HashMap::new(),
            swapchain_surfaces: HashMap::new(),
            group_visuals: HashMap::new(),
            backdrops: BackdropBlurs::default(),
            child_orders: HashMap::new(),
            raster,
            publication_ready,
            content_dirty: true,
            pixels_dirty: false,
            game_revision: 0,
        })
    }

    fn replace_rasterizer(&mut self, rasterizer: ShellRasterizer) -> Result<(), Box<dyn Error>> {
        self.raster =
            RasterDomain::new(&self.compositor, rasterizer, self.publication_ready.clone())?;
        self.content_dirty = true;
        Ok(())
    }

    pub(crate) fn replace_adapter(
        &mut self,
        adapter: AdapterLuid,
    ) -> Result<(AdapterLuid, IUnknown), Box<dyn Error>> {
        if self.raster.context.adapter == adapter {
            return Ok(self.device_context());
        }
        let rasterizer = pollster::block_on(ShellRasterizer::new_for_adapter(Some(adapter)))?;
        self.replace_rasterizer(rasterizer)?;
        Ok(self.device_context())
    }

    pub(crate) fn device_context(&self) -> (AdapterLuid, IUnknown) {
        (
            self.raster.context.adapter,
            self.raster.context.device.clone(),
        )
    }

    pub(crate) fn root_visual(&self) -> &ContainerVisual {
        &self.root
    }

    pub(crate) fn submit(&self, frame: ShellFrame) -> Option<u64> {
        self.raster.worker.submit(frame)
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.raster.worker.is_pending()
    }

    /// Takes the next finished raster result. Its chunk pixels are copied
    /// into their drawing surfaces only by the reconcile that commits it:
    /// drawing surface updates are applied by whichever commit comes next, so
    /// copying any earlier would let an unrelated commit show them before the
    /// layout they belong to.
    pub(crate) fn draw(&mut self) -> Result<Option<u64>, String> {
        if self.raster.pending.is_some() {
            return Ok(None);
        }
        let Some(result) = self.raster.worker.try_result() else {
            return Ok(None);
        };
        let revision = result.revision;
        self.raster.pending = Some(result);
        Ok(Some(revision))
    }

    fn apply_result(&mut self, result: ShellRasterResult) -> windows::core::Result<()> {
        let live = result
            .raster_chunks
            .iter()
            .map(|chunk| chunk.id)
            .collect::<HashSet<_>>();
        let surface_count = self.raster.surfaces.len();
        for (_, chunk) in self.raster.surfaces.extract_if(|id, _| !live.contains(id)) {
            self.raster.chunk_surfaces.retire(chunk);
        }
        let mut created = self.raster.surfaces.len() != surface_count;
        let pixels_changed = !result.updates.is_empty();
        for (id, resource) in result.updates {
            let (width, height) = (resource.width, resource.height);
            let surface = match self.raster.surfaces.entry(id) {
                std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    created = true;
                    entry.insert(self.raster.chunk_surfaces.create(width, height)?)
                }
            };
            // SAFETY: the resource's single owned reference is handed over,
            // and Vello left it in the unordered-access state.
            unsafe {
                self.raster.chunk_surfaces.update(
                    surface,
                    resource.into_abi_reference(),
                    width,
                    height,
                )?;
            }
        }
        self.raster.chunk_surfaces.flush()?;
        self.pixels_dirty |= pixels_changed;
        self.content_dirty |= created
            || self.raster.chunks != result.raster_chunks
            || self.raster.groups != result.composition_groups;
        self.raster.chunks = result.raster_chunks;
        self.raster.groups = result.composition_groups;
        Ok(())
    }

    fn content_update(
        &self,
        surfaces: &[SurfaceLayer],
        game_revision: u64,
        device: &RenderDevice,
    ) -> Option<CompositionContents> {
        if !self.content_dirty && self.game_revision == game_revision {
            return None;
        }
        let group_bounds = self
            .raster
            .groups
            .iter()
            .map(|group| (group.id, group.bounds))
            .collect::<HashMap<_, _>>();
        let groups = self
            .raster
            .groups
            .iter()
            .map(|group| {
                let origin = group
                    .parent
                    .and_then(|parent| group_bounds.get(&parent))
                    .map_or((0.0, 0.0), |bounds| (bounds.x0, bounds.y0));
                CompositionGroup {
                    id: group.id,
                    parent: group.parent,
                    z_order: group.z_order,
                    x: (group.bounds.x0 - origin.0) as f32,
                    y: (group.bounds.y0 - origin.1) as f32,
                    width: group.bounds.width() as f32,
                    height: group.bounds.height() as f32,
                    opacity: group.opacity,
                    shadow: group.shadow,
                    backdrop_blur: group.backdrop_blur,
                }
            })
            .collect();
        let mut layers = Vec::new();
        for chunk in &self.raster.chunks {
            let Some(surface) = self
                .raster
                .surfaces
                .get(&chunk.id)
                .and_then(|surface| surface.composition_surface().ok())
            else {
                continue;
            };
            let origin = chunk
                .group
                .and_then(|group| group_bounds.get(&group))
                .map_or((0.0, 0.0), |bounds| (bounds.x0, bounds.y0));
            layers.push(CompositionLayer {
                id: CompositionLayerId::Raster(chunk.id),
                parent: chunk.group,
                z_order: chunk.z_order,
                content: CompositionLayerContent::Surface(surface),
                x: chunk.bounds.x0 as f32 - origin.0 as f32,
                y: chunk.bounds.y0 as f32 - origin.1 as f32,
                width: chunk.bounds.width() as f32,
                height: chunk.bounds.height() as f32,
                opacity: 1.0,
                stretch: CompositionLayerStretch::None,
                horizontal_alignment: 0.0,
                vertical_alignment: 0.0,
            });
        }
        for surface in surfaces {
            let Some(content) = device.content(surface.connection) else {
                continue;
            };
            let fitted = surface.placement;
            let cropped_width = 1.0 - (fitted.source[0] + (1.0 - fitted.source[2]));
            let cropped_height = 1.0 - (fitted.source[1] + (1.0 - fitted.source[3]));
            let horizontal_alignment = if cropped_width < 1.0 {
                (fitted.source[0] / (1.0 - cropped_width)).clamp(0.0, 1.0)
            } else {
                0.5
            };
            let vertical_alignment = if cropped_height < 1.0 {
                (fitted.source[1] / (1.0 - cropped_height)).clamp(0.0, 1.0)
            } else {
                0.5
            };
            let origin = surface
                .parent
                .and_then(|group| group_bounds.get(&group))
                .map_or((0.0, 0.0), |bounds| (bounds.x0, bounds.y0));
            layers.push(CompositionLayer {
                id: CompositionLayerId::Surface(surface.connection, surface.instance),
                parent: surface.parent,
                z_order: surface.z_order,
                content: CompositionLayerContent::Swapchain(content),
                x: fitted.destination[0] - origin.0 as f32,
                y: fitted.destination[1] - origin.1 as f32,
                width: (fitted.destination[2] - fitted.destination[0]).max(0.0),
                height: (fitted.destination[3] - fitted.destination[1]).max(0.0),
                opacity: fitted.opacity,
                stretch: if fitted.source == [0.0, 0.0, 1.0, 1.0] {
                    CompositionLayerStretch::Fill
                } else {
                    CompositionLayerStretch::UniformToFill
                },
                horizontal_alignment,
                vertical_alignment,
            });
        }
        layers.sort_by_key(|layer| layer.z_order);
        Some(CompositionContents { groups, layers })
    }

    pub(crate) fn reconcile(
        &mut self,
        surfaces: &[SurfaceLayer],
        game_revision: u64,
        device: &RenderDevice,
    ) -> Result<bool, Box<dyn Error>> {
        if let Some(result) = self.raster.pending.take() {
            let applied = self.apply_result(result);
            self.raster.worker.acknowledge_publication();
            applied?;
        }
        let pixels_dirty = std::mem::take(&mut self.pixels_dirty);
        let Some(content) = self.content_update(surfaces, game_revision, device) else {
            return Ok(pixels_dirty);
        };
        self.reconcile_content(&content)?;

        self.content_dirty = false;
        self.game_revision = game_revision;
        Ok(true)
    }

    fn reconcile_content(&mut self, content: &CompositionContents) -> windows::core::Result<()> {
        for group in &content.groups {
            let retained = if let Some(group) = self.group_visuals.get(&group.id) {
                CompositionGroupVisual {
                    visual: group.visual.clone(),
                    clip: group.clip.clone(),
                    shadow: group.shadow.clone(),
                }
            } else {
                let visual = self.compositor.CreateLayerVisual()?;
                let clip = self.compositor.CreateInsetClip()?;
                visual.SetClip(&clip)?;
                self.group_visuals.insert(
                    group.id,
                    CompositionGroupVisual {
                        visual: visual.clone(),
                        clip: clip.clone(),
                        shadow: None,
                    },
                );
                CompositionGroupVisual {
                    visual,
                    clip,
                    shadow: None,
                }
            };
            retained.visual.SetOffset(Vector3 {
                X: group.x,
                Y: group.y,
                Z: 0.0,
            })?;
            retained.visual.SetSize(Vector2 {
                X: group.width.max(0.0),
                Y: group.height.max(0.0),
            })?;
            retained.visual.SetOpacity(group.opacity.clamp(0.0, 1.0))?;
            retained.clip.SetLeftInset(0.0)?;
            retained.clip.SetTopInset(0.0)?;
            retained.clip.SetRightInset(0.0)?;
            retained.clip.SetBottomInset(0.0)?;

            let shadow = if let Some(style) = group.shadow {
                let shadow = match retained.shadow {
                    Some(shadow) => shadow,
                    None => {
                        let shadow = self.compositor.CreateDropShadow()?;
                        shadow.SetSourcePolicy(
                            CompositionDropShadowSourcePolicy::InheritFromVisualContent,
                        )?;
                        shadow
                    }
                };
                shadow.SetBlurRadius(style.blur_radius.max(0.0))?;
                shadow.SetColor(windows::UI::Color {
                    A: style.color[3],
                    R: style.color[0],
                    G: style.color[1],
                    B: style.color[2],
                })?;
                shadow.SetOffset(Vector3 {
                    X: style.offset[0],
                    Y: style.offset[1],
                    Z: 0.0,
                })?;
                retained.visual.SetShadow(&shadow)?;
                Some(shadow)
            } else {
                if retained.shadow.is_some() {
                    retained.visual.SetShadow(None::<&CompositionShadow>)?;
                }
                None
            };
            if let Some(stored) = self.group_visuals.get_mut(&group.id) {
                stored.shadow = shadow;
            }
            if let Some(blur) = group.backdrop_blur {
                let backdrop = self.backdrops.update(&self.compositor, group.id, blur)?;
                backdrop.SetOffset(Vector3 {
                    X: group.x,
                    Y: group.y,
                    Z: 0.0,
                })?;
                backdrop.SetSize(Vector2 {
                    X: group.width.max(0.0),
                    Y: group.height.max(0.0),
                })?;
            }
        }

        for layer in &content.layers {
            let content_identity = layer.content.identity();
            let retained = if let Some(visual) = self.visuals.get(&layer.id) {
                CompositionVisual {
                    visual: visual.visual.clone(),
                    brush: visual.brush.clone(),
                    content_identity: visual.content_identity,
                }
            } else {
                let visual = self.compositor.CreateSpriteVisual()?;
                let brush = self.layer_brush(&layer.content)?;
                visual.SetBrush(&brush)?;
                self.visuals.insert(
                    layer.id,
                    CompositionVisual {
                        visual: visual.clone(),
                        brush: brush.clone(),
                        content_identity,
                    },
                );
                CompositionVisual {
                    visual,
                    brush,
                    content_identity,
                }
            };
            let brush = if retained.content_identity == content_identity {
                retained.brush
            } else {
                let brush = self.layer_brush(&layer.content)?;
                retained.visual.SetBrush(&brush)?;
                if let Some(stored) = self.visuals.get_mut(&layer.id) {
                    stored.brush = brush.clone();
                    stored.content_identity = content_identity;
                }
                brush
            };
            brush.SetStretch(match layer.stretch {
                CompositionLayerStretch::None => CompositionStretch::None,
                CompositionLayerStretch::Fill => CompositionStretch::Fill,
                CompositionLayerStretch::UniformToFill => CompositionStretch::UniformToFill,
            })?;
            brush.SetHorizontalAlignmentRatio(layer.horizontal_alignment)?;
            brush.SetVerticalAlignmentRatio(layer.vertical_alignment)?;
            retained.visual.SetOffset(Vector3 {
                X: layer.x,
                Y: layer.y,
                Z: 0.0,
            })?;
            retained.visual.SetSize(Vector2 {
                X: layer.width.max(0.0),
                Y: layer.height.max(0.0),
            })?;
            retained.visual.SetOpacity(layer.opacity.clamp(0.0, 1.0))?;
        }

        let mut desired = HashMap::<Option<u64>, Vec<(i32, CompositionNodeId)>>::new();
        for group in &content.groups {
            let children = desired.entry(group.parent).or_default();
            if group.backdrop_blur.is_some() {
                children.push((group.z_order, CompositionNodeId::Backdrop(group.id)));
            }
            children.push((group.z_order, CompositionNodeId::Group(group.id)));
        }
        for layer in &content.layers {
            desired
                .entry(layer.parent)
                .or_default()
                .push((layer.z_order, CompositionNodeId::Layer(layer.id)));
        }
        let desired = desired
            .into_iter()
            .map(|(parent, mut children)| {
                children.sort_by_key(|(z_order, _)| *z_order);
                (
                    parent,
                    children.into_iter().map(|(_, child)| child).collect(),
                )
            })
            .collect::<HashMap<_, Vec<_>>>();
        let changed_parents = self
            .child_orders
            .keys()
            .chain(desired.keys())
            .copied()
            .collect::<HashSet<_>>()
            .into_iter()
            .filter(|parent| self.child_orders.get(parent) != desired.get(parent))
            .collect::<Vec<_>>();

        for parent in &changed_parents {
            if let Some(parent) = parent {
                if let Some(group) = self.group_visuals.get(parent) {
                    group.visual.Children()?.RemoveAll()?;
                }
            } else {
                self.root.Children()?.RemoveAll()?;
            }
        }
        for parent in changed_parents {
            let next = desired.get(&parent).cloned().unwrap_or_default();
            let children = if let Some(parent) = parent {
                self.group_visuals
                    .get(&parent)
                    .map(|group| group.visual.Children())
                    .transpose()?
            } else {
                Some(self.root.Children()?)
            };
            if let Some(children) = children {
                for child in &next {
                    if let Some(visual) = self.node_visual(*child) {
                        children.InsertAtTop(&visual)?;
                    }
                }
            }
        }
        self.child_orders = desired;

        let live_groups = content
            .groups
            .iter()
            .map(|group| group.id)
            .collect::<HashSet<_>>();
        let live_layers = content
            .layers
            .iter()
            .map(|layer| layer.id)
            .collect::<HashSet<_>>();
        self.group_visuals.retain(|id, _| live_groups.contains(id));
        let blurred = content
            .groups
            .iter()
            .filter(|group| group.backdrop_blur.is_some())
            .map(|group| group.id)
            .collect::<HashSet<_>>();
        self.backdrops.retain(|id| blurred.contains(&id));
        self.visuals.retain(|id, _| live_layers.contains(id));
        let live_contents = content
            .layers
            .iter()
            .map(|layer| layer.content.identity())
            .collect::<HashSet<_>>();
        self.swapchain_surfaces
            .retain(|identity, _| live_contents.contains(identity));
        Ok(())
    }

    fn node_visual(&self, id: CompositionNodeId) -> Option<Visual> {
        match id {
            CompositionNodeId::Backdrop(id) => self.backdrops.visual(id)?.cast().ok(),
            CompositionNodeId::Group(id) => self.group_visuals.get(&id)?.visual.cast().ok(),
            CompositionNodeId::Layer(id) => self.visuals.get(&id)?.visual.cast().ok(),
        }
    }

    fn surface_brush(
        &mut self,
        swapchain: &IUnknown,
    ) -> windows::core::Result<CompositionSurfaceBrush> {
        let identity = swapchain.as_raw() as usize;
        let surface = match self.swapchain_surfaces.get(&identity) {
            Some((_, surface)) => surface.clone(),
            None => {
                let interop: ICompositorInterop = self.compositor.cast()?;
                // SAFETY: captured swapchains are retained by GameHost while their
                // corresponding visual is installed in this tree.
                let surface = unsafe { interop.CreateCompositionSurfaceForSwapChain(swapchain)? };
                self.swapchain_surfaces
                    .insert(identity, (swapchain.clone(), surface.clone()));
                surface
            }
        };
        let brush = self.compositor.CreateSurfaceBrushWithSurface(&surface)?;
        brush.SetStretch(CompositionStretch::Fill)?;
        Ok(brush)
    }

    fn layer_brush(
        &mut self,
        content: &CompositionLayerContent,
    ) -> windows::core::Result<CompositionSurfaceBrush> {
        match content {
            CompositionLayerContent::Swapchain(swapchain) => self.surface_brush(swapchain),
            CompositionLayerContent::Surface(surface) => {
                let brush = self.compositor.CreateSurfaceBrushWithSurface(surface)?;
                brush.SetStretch(CompositionStretch::Fill)?;
                Ok(brush)
            }
        }
    }
}
