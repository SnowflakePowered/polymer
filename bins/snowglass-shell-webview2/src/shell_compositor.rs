//! Retained shell raster and WUC composition ownership.

mod chrome;
mod raster;

pub(crate) use raster::ShellRasterizer;

use crate::{
    game_host::GameHost,
    shell_document::CompositionShadowStyle,
    shell_dom::{
        RasterChunkId, RasterChunkLayout, ShellCompositionGroup, ShellFrame, TrackedSurfaceLayout,
    },
};
use raster::{ShellRasterResult, ShellRasterWorker};
use snowglass_protocol::AdapterLuid;
use snowglass_renderer::{Extent, RasterSurface};
use std::{
    collections::{HashMap, HashSet},
    error::Error,
};
use windows::{
    UI::Composition::{
        CompositionShadow, CompositionStretch, CompositionSurfaceBrush, Compositor,
        ContainerVisual, DropShadow, InsetClip, LayerVisual, SpriteVisual, Visual,
    },
    Win32::System::WinRT::Composition::ICompositorInterop,
    core::{IUnknown, Interface},
};
use windows_numerics::{Vector2, Vector3};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum CompositionLayerId {
    Raster(RasterChunkId),
    Surface(u64),
}

#[derive(Clone, Copy)]
pub(crate) enum CompositionLayerStretch {
    None,
    Fill,
    UniformToFill,
}

pub(crate) enum CompositionLayerContent {
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

pub(crate) struct CompositionLayer {
    pub id: CompositionLayerId,
    pub parent: Option<u64>,
    pub z_order: i32,
    pub content: CompositionLayerContent,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub opacity: f32,
    pub stretch: CompositionLayerStretch,
    pub horizontal_alignment: f32,
    pub vertical_alignment: f32,
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
}

struct CompositionContents {
    groups: Vec<CompositionGroup>,
    layers: Vec<CompositionLayer>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum CompositionNodeId {
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

#[derive(Clone)]
pub(crate) struct ShellDeviceContext {
    pub adapter: AdapterLuid,
    pub device: IUnknown,
    pub queue: IUnknown,
}

struct PendingPublication {
    chunks: Vec<RasterChunkLayout>,
    groups: Vec<ShellCompositionGroup>,
    tracked: Vec<TrackedSurfaceLayout>,
}

/// Owns every shell-specific GPU and retained-composition resource. GameHost
/// supplies captured-surface snapshots but never translates the shell tree.
pub(crate) struct ShellCompositor {
    compositor: Compositor,
    root: ContainerVisual,
    visuals: HashMap<CompositionLayerId, CompositionVisual>,
    group_visuals: HashMap<u64, CompositionGroupVisual>,
    child_orders: HashMap<Option<u64>, Vec<CompositionNodeId>>,
    context: ShellDeviceContext,
    worker: ShellRasterWorker,
    surfaces: Vec<RasterSurface>,
    slots: HashMap<RasterChunkId, usize>,
    chunks: Vec<RasterChunkLayout>,
    groups: Vec<ShellCompositionGroup>,
    dirty: HashSet<RasterChunkId>,
    pending: Option<PendingPublication>,
    content_dirty: bool,
}

impl ShellCompositor {
    pub(crate) fn new(
        compositor: &Compositor,
        rasterizer: ShellRasterizer,
    ) -> Result<Self, Box<dyn Error>> {
        let context = rasterizer.device_context()?;
        let adapter = context.adapter;
        let anchor = unsafe {
            RasterSurface::from_external(
                compositor,
                adapter,
                context.device.clone().into_raw(),
                context.queue.clone().into_raw(),
                Extent::new(64, 64),
            )?
        };
        let root = compositor.CreateContainerVisual()?;
        root.SetRelativeSizeAdjustment(Vector2 { X: 1.0, Y: 1.0 })?;
        Ok(Self {
            compositor: compositor.clone(),
            root,
            visuals: HashMap::new(),
            group_visuals: HashMap::new(),
            child_orders: HashMap::new(),
            context,
            worker: ShellRasterWorker::spawn(rasterizer),
            surfaces: vec![anchor],
            slots: HashMap::new(),
            chunks: Vec::new(),
            groups: Vec::new(),
            dirty: HashSet::new(),
            pending: None,
            content_dirty: true,
        })
    }

    pub(crate) fn replace_rasterizer(
        &mut self,
        rasterizer: ShellRasterizer,
    ) -> Result<(), Box<dyn Error>> {
        let replacement = Self::new(&self.compositor, rasterizer)?;
        self.context = replacement.context;
        self.worker = replacement.worker;
        self.surfaces = replacement.surfaces;
        self.slots.clear();
        self.chunks.clear();
        self.groups.clear();
        self.dirty.clear();
        self.pending = None;
        self.content_dirty = true;
        Ok(())
    }

    pub(crate) fn device_context(&self) -> ShellDeviceContext {
        self.context.clone()
    }

    pub(crate) fn root_visual(&self) -> &ContainerVisual {
        &self.root
    }

    pub(crate) fn submit(&self, frame: ShellFrame) {
        self.worker.submit(frame);
    }

    pub(crate) fn is_pending(&self) -> bool {
        !self.dirty.is_empty() || self.worker.is_pending()
    }

    pub(crate) fn draw(&mut self) -> Result<Option<Vec<TrackedSurfaceLayout>>, String> {
        while let Some(result) = self.worker.try_result() {
            self.apply_result(result)?;
        }
        for id in self.dirty.iter().copied().collect::<Vec<_>>() {
            let Some(slot) = self.slots.get(&id).copied() else {
                self.dirty.remove(&id);
                continue;
            };
            if !self.surfaces[slot]
                .try_present()
                .map_err(|error| error.to_string())?
            {
                continue;
            }
            self.dirty.remove(&id);
        }
        if !self.dirty.is_empty() {
            return Ok(None);
        }
        let Some(pending) = self.pending.take() else {
            return Ok(None);
        };
        self.content_dirty |= self.chunks != pending.chunks || self.groups != pending.groups;
        self.chunks = pending.chunks;
        self.groups = pending.groups;
        self.worker.acknowledge_publication();
        Ok(Some(pending.tracked))
    }

    fn apply_result(&mut self, result: ShellRasterResult) -> Result<(), String> {
        debug_assert!(self.pending.is_none());
        let live = result
            .raster_chunks
            .iter()
            .map(|chunk| chunk.id)
            .collect::<HashSet<_>>();
        self.slots.retain(|id, _| live.contains(id));
        let mut used = self.slots.values().copied().collect::<HashSet<_>>();
        for chunk in &result.raster_chunks {
            if self.slots.contains_key(&chunk.id) {
                continue;
            }
            let slot = (0..self.surfaces.len())
                .find(|slot| !used.contains(slot))
                .unwrap_or(self.surfaces.len());
            if slot == self.surfaces.len() {
                let surface = unsafe {
                    RasterSurface::from_external(
                        &self.compositor,
                        self.context.adapter,
                        self.context.device.clone().into_raw(),
                        self.context.queue.clone().into_raw(),
                        chunk_capacity(chunk),
                    )
                }
                .map_err(|error| error.to_string())?;
                self.surfaces.push(surface);
            }
            self.slots.insert(chunk.id, slot);
            used.insert(slot);
            self.content_dirty = true;
        }
        for chunk in &result.raster_chunks {
            let slot = self.slots[&chunk.id];
            let desired = chunk_capacity(chunk);
            let current = self.surfaces[slot].capacity();
            if desired.width > current.width
                || desired.height > current.height
                || u64::from(current.width) * u64::from(current.height)
                    > 4 * u64::from(desired.width) * u64::from(desired.height)
            {
                self.surfaces[slot]
                    .resize(desired)
                    .map_err(|error| error.to_string())?;
                self.dirty.insert(chunk.id);
            }
        }
        for (id, resource) in result.updates {
            let Some(slot) = self.slots.get(&id).copied() else {
                continue;
            };
            let (width, height) = (resource.width, resource.height);
            unsafe {
                self.surfaces[slot].set_source(resource.into_abi_reference(), width, height);
            }
            self.dirty.insert(id);
        }
        self.pending = Some(PendingPublication {
            chunks: result.raster_chunks,
            groups: result.composition_groups,
            tracked: result.surface_layout,
        });
        Ok(())
    }

    fn content_update(
        &mut self,
        game: &mut GameHost,
    ) -> Result<Option<CompositionContents>, Box<dyn Error>> {
        let game_dirty = game.take_composition_dirty();
        if !self.content_dirty && !game_dirty {
            return Ok(None);
        }
        self.content_dirty = false;
        let group_bounds = self
            .groups
            .iter()
            .map(|group| (group.id, group.bounds))
            .collect::<HashMap<_, _>>();
        let groups = self
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
                }
            })
            .collect();
        let mut layers = Vec::new();
        for chunk in &self.chunks {
            let Some(slot) = self.slots.get(&chunk.id).copied() else {
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
                content: CompositionLayerContent::Surface(
                    self.surfaces[slot].composition_surface(),
                ),
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
        layers.extend(game.composition_layers(&group_bounds)?);
        layers.sort_by_key(|layer| layer.z_order);
        Ok(Some(CompositionContents { groups, layers }))
    }

    pub(crate) fn reconcile(&mut self, game: &mut GameHost) -> Result<bool, Box<dyn Error>> {
        let Some(content) = self.content_update(game)? else {
            return Ok(false);
        };
        self.reconcile_content(&content)?;
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
                    None => self.compositor.CreateDropShadow()?,
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
            desired
                .entry(group.parent)
                .or_default()
                .push((group.z_order, CompositionNodeId::Group(group.id)));
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
        self.visuals.retain(|id, _| live_layers.contains(id));
        Ok(())
    }

    fn node_visual(&self, id: CompositionNodeId) -> Option<Visual> {
        match id {
            CompositionNodeId::Group(id) => self.group_visuals.get(&id)?.visual.cast().ok(),
            CompositionNodeId::Layer(id) => self.visuals.get(&id)?.visual.cast().ok(),
        }
    }

    fn surface_brush(
        &self,
        swapchain: &IUnknown,
    ) -> windows::core::Result<CompositionSurfaceBrush> {
        let interop: ICompositorInterop = self.compositor.cast()?;
        // SAFETY: captured swapchains are retained by GameHost while their
        // corresponding visual is installed in this tree.
        let surface = unsafe { interop.CreateCompositionSurfaceForSwapChain(swapchain)? };
        let brush = self.compositor.CreateSurfaceBrushWithSurface(&surface)?;
        brush.SetStretch(CompositionStretch::Fill)?;
        Ok(brush)
    }

    fn layer_brush(
        &self,
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

fn chunk_capacity(chunk: &RasterChunkLayout) -> Extent {
    fn bucket(value: u32) -> u32 {
        value.max(64).checked_next_power_of_two().unwrap_or(value)
    }
    Extent::new(
        bucket(chunk.bounds.width().ceil().max(1.0) as u32),
        bucket(chunk.bounds.height().ceil().max(1.0) as u32),
    )
}
