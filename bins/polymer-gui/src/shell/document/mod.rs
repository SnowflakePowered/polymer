//! Lays out and paints the sole shell document with Blitz.
//!
//! Ordinary Solid UI, bezel art, and guest-frame placeholders all live in
//! this document and are composited without a browser process.
//!
//! A game view is written as `<tracked-surface src="N">`, where N is the
//! tracked render target. Painting diverts those into rectangles
//! ([`bands::GamePlacement`])
//! and splits the remaining chrome into bands, which the compositor stacks
//! around them.

mod bands;
mod events;
mod hit_test;
mod image;
mod image_levels;
mod media;
mod mutations;
mod providers;
mod scrolling;
mod shaders;
mod tracked_surface;
mod urls;

use crate::shell::protocol::{
    ShellFrame, ShellInbox, SurfaceGroupBounds, TrackedSource, TrackedSurfaceLayout,
};
use crate::shell::runtime::ShellRuntime;
use crate::shell::runtime::deno::ScrollCommand;
use anyrender::PaintScene as _;
use bands::BandedScene;
pub use bands::{GamePlacement, GameViewStyle};
use blitz_dom::NodeId;
use blitz_dom::local_name;
use blitz_traits::net::{NetProvider, NetWaker};
pub use events::{DomKeyboardEvent, DomMouseEvent, DomWheelEvent};
use hit_test::{node_is_within_tracked_surface, pointer_hit};
use image_levels::ImageLevels;
pub(crate) use media::{MediaEvents, MediaPlayers, MediaSignal, MediaState};
use polymer_composition::{RasterChunkId, ShellCompositionGroup, ShellRasterChunk};
use providers::NativeShellProvider;
pub(crate) use shaders::{LoadedPreset, PresetOptions, PresetParameter, ShaderPresets};
use std::{
    cell::{Ref, RefCell, RefMut},
    collections::HashMap,
    rc::Rc,
    sync::{
        Arc,
        mpsc::{Receiver, channel},
    },
};
use tracked_surface::{
    apply_tracked_surface_styles, enclosing_surface_group, initialise_tracked_surfaces,
    is_tracked_surface_element,
};
pub(crate) use urls::{ShellUrls, ThemeLocation};

/// How many times a paint will re-resolve to pick up assets that only became
/// reachable through an asset it just loaded.
const ASSET_ROUNDS: usize = 4;
const ANIMATION_FRAME_INTERVAL: std::time::Duration = std::time::Duration::from_micros(16_667);

fn chrome_fragment_bounds(bounds: Option<kurbo::Rect>, viewport: kurbo::Rect) -> kurbo::Rect {
    // The desktop visual tree clips at the window. Keeping a promoted group's
    // full viewport-space bounds here is essential: clamping its origin at an
    // edge would move the chrome into a different coordinate space from an
    // enclosed tracked surface.
    let bounds = bounds
        .map(|bounds| bounds.inflate(1.0, 1.0))
        .unwrap_or(viewport);
    kurbo::Rect::new(
        bounds.x0.floor(),
        bounds.y0.floor(),
        bounds.x1.ceil(),
        bounds.y1.ceil(),
    )
}

fn normalized_scale_factor(scale_factor: f32) -> f32 {
    if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    }
}

fn scaled_rect(rect: kurbo::Rect, scale_factor: f64) -> kurbo::Rect {
    kurbo::Rect::new(
        rect.x0 * scale_factor,
        rect.y0 * scale_factor,
        rect.x1 * scale_factor,
        rect.y1 * scale_factor,
    )
}

/// Set to report what each repaint of this layer costs.
const PAINT_TIMING: &str = "POLYMER_GAME_LAYER_TIMING";

pub struct ShellDocument {
    document: blitz_html::HtmlDocument,
    /// Changes only when the connected DOM tree changes shape. Paints,
    /// pseudo-class changes, and attribute updates must not make DevTools
    /// discard its expanded tree and current selection.
    dom_structure_revision: u64,
    /// Assets arrive here because a fetch is issued while the document is
    /// mid-resolve and cannot take them straight away.
    arrivals: Receiver<()>,
    /// Backend-neutral paint commands per band. Vello path and glyph encoding
    /// happens on the latest-only raster worker so DOM input and captured
    /// surface presentation never wait for framebuffer-sized scene encoding.
    bands: Vec<anyrender::Scene>,
    placements: Vec<GamePlacement>,
    images: ImageLevels,
    image_loader: Arc<image::ImageLoader>,
    media: MediaPlayers,
    shaders: ShaderPresets,
    urls: Arc<ShellUrls>,
    width: u32,
    height: u32,
    scale_factor: f32,
    /// Set whenever layout or content changed, so a paint is only spent when
    /// something can differ. Guest frames arriving do not dirty this layer.
    dirty: bool,
    /// Style/tree mutations need Stylo and Taffy.
    needs_resolve: bool,
    /// Geometry snapshots are only needed when layout may have changed.
    /// Paint-only declarations such as transforms still resolve and repaint,
    /// but script derives their translated geometry from the inline style.
    needs_geometry_sync: bool,
    scroll_animating: bool,
    animation_epoch: std::time::Instant,
    last_animation_frame: std::time::Instant,
    pending_scrolls: Vec<(NodeId, ScrollCommand)>,
    /// Paint-order hover path used only while Blitz's native hit target
    /// differs from the compositor-visible target.
    pointer_hover_path: Vec<NodeId>,
}

/// Result of hit testing the document for one input event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ShellHitSnapshot {
    pub node_id: NodeId,
    pub over_tracked_surface: bool,
}

#[derive(Clone)]
pub(crate) struct SharedShellDocument(pub(crate) Rc<RefCell<ShellDocument>>);

impl blitz_dom::Document for SharedShellDocument {
    fn inner(&self) -> blitz_dom::DocGuard<'_> {
        blitz_dom::DocGuard::RefCell(Ref::map(self.0.borrow(), |shell| shell.document.as_ref()))
    }

    fn inner_mut(&mut self) -> blitz_dom::DocGuardMut<'_> {
        blitz_dom::DocGuardMut::RefCell(RefMut::map(self.0.borrow_mut(), |shell| {
            shell.document.as_mut()
        }))
    }
}

impl SharedShellDocument {
    pub fn produce_frame(&self, runtime: &mut ShellRuntime) -> Option<ShellFrame> {
        let frame = self.0.borrow_mut().produce_frame(runtime);
        runtime.publish_layout();
        frame
    }

    pub fn resize(&self, runtime: &mut ShellRuntime, width: u32, height: u32, scale_factor: f32) {
        let (logical_width, logical_height, scale_factor) = {
            let mut document = self.0.borrow_mut();
            document.resize(width, height, scale_factor);
            let (logical_width, logical_height) = document.logical_size();
            (logical_width, logical_height, document.scale_factor())
        };
        runtime.resize(logical_width, logical_height, scale_factor);
    }
}

/// Factory for a shell document and its native network/window providers.
///
/// Asset arrivals and redraw requests wake the shell thread. Cursor, title
/// and IME changes go straight to the window thread.
#[derive(Clone)]
pub struct ShellDocumentFactory {
    event_proxy: crate::window_manager::EventLoopProxy<crate::app::UserEvent>,
    directories: polymer_library::Directories,
}

impl ShellDocumentFactory {
    pub fn new(
        event_proxy: crate::window_manager::EventLoopProxy<crate::app::UserEvent>,
        directories: polymer_library::Directories,
    ) -> Self {
        Self {
            event_proxy,
            directories,
        }
    }

    pub(in crate::shell) fn create(
        &self,
        source: &crate::shell::ShellSource,
        shell: ShellInbox,
        outbox: crate::shell::protocol::ShellOutbox,
    ) -> ShellDocument {
        let base_url = source.base_url.clone();
        let (sender, arrivals) = channel();
        let arrival_shell = shell.clone();
        let waker: Arc<dyn NetWaker> = Arc::new(move |_| {
            let _ = sender.send(());
            arrival_shell.wake();
        });
        let urls = Arc::new(ShellUrls::new(
            &base_url,
            source.theme.clone(),
            self.directories.clone(),
        ));
        let net: Arc<dyn NetProvider> = Arc::new(providers::ShellNetProvider::new(
            waker.clone(),
            urls.clone(),
        ));
        let level_shell = shell.clone();
        let images = ImageLevels::new(move || level_shell.wake());
        let media_shell = shell.clone();
        let shell_provider: Arc<dyn blitz_traits::shell::ShellProvider> =
            Arc::new(NativeShellProvider::new(self.event_proxy.clone(), shell));
        let mut document = blitz_html::HtmlDocument::from_html(
            &source.html,
            blitz_dom::DocumentConfig {
                base_url: Some(base_url.clone()),
                net_provider: Some(net.clone()),
                shell_provider: Some(shell_provider),
                html_parser_provider: Some(Arc::new(blitz_html::HtmlProvider)),
                ..Default::default()
            },
        );
        let shaders = ShaderPresets::new(outbox.clone());
        let media = MediaPlayers::new(
            net.clone(),
            document.as_ref().id(),
            &base_url,
            media_shell,
            outbox,
            urls.clone(),
        );
        let image_loader = Arc::new(image::ImageLoader::new(
            net,
            waker,
            document.as_ref().id(),
            &base_url,
        ));
        initialise_tracked_surfaces(&mut document);
        let mut document = ShellDocument {
            document,
            dom_structure_revision: 1,
            arrivals,
            bands: vec![anyrender::Scene::new()],
            placements: Vec::new(),
            images,
            image_loader,
            media,
            shaders,
            urls,
            width: 0,
            height: 0,
            scale_factor: 1.0,
            dirty: true,
            needs_resolve: true,
            needs_geometry_sync: true,
            scroll_animating: false,
            animation_epoch: std::time::Instant::now(),
            last_animation_frame: std::time::Instant::now(),
            pending_scrolls: Vec::new(),
            pointer_hover_path: Vec::new(),
        };
        document.resize(source.width, source.height, source.scale_factor);
        document.load_document_media();
        // Scripts run after parsing, with a complete DOM just like deferred
        // document scripts. Resolve once first so geometry APIs have useful
        // values during initialisation.
        document.document.as_mut().resolve(0.0);
        document
    }
}

impl ShellDocument {
    pub(crate) fn urls(&self) -> Arc<ShellUrls> {
        self.urls.clone()
    }

    pub(crate) fn runtime_context(&self) -> (&blitz_dom::BaseDocument, u32, u32, f32) {
        let (width, height) = self.logical_size();
        (self.document.as_ref(), width, height, self.scale_factor)
    }

    pub(crate) fn base(&self) -> &blitz_dom::BaseDocument {
        self.document.as_ref()
    }

    pub(crate) fn is_script_node(&self, node: NodeId) -> bool {
        let document = self.document.as_ref();
        let anonymous =
            |node: Option<&blitz_dom::Node>| node.is_some_and(blitz_dom::Node::is_anonymous);
        let Some(current) = document.get_node(node) else {
            return false;
        };
        !anonymous(Some(current))
            && !anonymous(current.parent.and_then(|parent| document.get_node(parent)))
    }

    pub(crate) fn logical_size(&self) -> (u32, u32) {
        let scale = f64::from(self.scale_factor);
        (
            (f64::from(self.width) / scale).round().max(1.0) as u32,
            (f64::from(self.height) / scale).round().max(1.0) as u32,
        )
    }

    pub(crate) fn scale_factor(&self) -> f32 {
        self.scale_factor
    }

    pub(crate) fn physical_to_css(&self, x: f32, y: f32) -> (f32, f32) {
        (x / self.scale_factor, y / self.scale_factor)
    }

    /// Evicts every image mip chain. Images still shown load again.
    pub(crate) fn clear_image_mip_chains(&mut self) {
        polymer_mipmap::MipImages::clear();
        self.dirty = true;
    }

    pub fn resize(&mut self, width: u32, height: u32, scale_factor: f32) {
        let (width, height) = (width.max(1), height.max(1));
        let scale_factor = normalized_scale_factor(scale_factor);
        if (self.width, self.height, self.scale_factor) == (width, height, scale_factor) {
            return;
        }
        self.width = width;
        self.height = height;
        self.scale_factor = scale_factor;
        self.image_loader.set_scale_factor(scale_factor);
        let (logical_width, logical_height) = self.logical_size();
        // Absolutely positioned boxes resolve against the viewport, so without
        // one every box in this layer lays out at zero size. Keep Blitz in CSS
        // pixels and scale its completed display list once below. Blitz 0.3's
        // internal HiDPI transform path scales layout translations but not
        // absolute lengths inside CSS transforms, which otherwise separates
        // transformed chrome from its tracked surface.
        self.document
            .as_mut()
            .set_viewport(blitz_traits::shell::Viewport::new(
                logical_width,
                logical_height,
                1.0,
                blitz_traits::shell::ColorScheme::Dark,
            ));
        self.dirty = true;
        self.needs_resolve = true;
        self.needs_geometry_sync = true;
    }

    /// Forces the next paint even when the document itself did not change.
    ///
    /// This is also needed after the merged compositor moves to another GPU,
    /// because the replacement device must create fresh local chrome textures.
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn needs_paint(&self) -> bool {
        self.dirty || self.animation_frame_due()
    }

    /// When the next animation frame is due, while Blitz is animating.
    pub fn next_animation_frame(&self) -> Option<std::time::Instant> {
        self.document
            .as_ref()
            .is_animating()
            .then(|| self.last_animation_frame + ANIMATION_FRAME_INTERVAL)
    }

    fn animation_frame_due(&self) -> bool {
        self.document.as_ref().is_animating()
            && self.last_animation_frame.elapsed() >= ANIMATION_FRAME_INTERVAL
    }

    fn animation_time(&self) -> f64 {
        self.animation_epoch.elapsed().as_secs_f64()
    }

    pub fn hit_test(&self, x: f32, y: f32) -> Option<ShellHitSnapshot> {
        let painted = pointer_hit(self.document.as_ref(), x, y)?;
        // Blitz may return generated paint/layout elements which have no
        // identity-preserving JavaScript wrapper. Dispatching one of those
        // nodes makes the Deno bridge fall back to `document`, so delegated
        // DOM handlers never see the button underneath. Browser event
        // targets are DOM nodes: promote the paint hit to the nearest mapped
        // DOM ancestor before handing it to the event driver.
        let target = std::iter::successors(Some(painted), |node| {
            self.document
                .as_ref()
                .get_node(*node)
                .and_then(|node| node.parent)
        })
        .find(|node| self.is_script_node(*node))?;
        let over_tracked_surface = node_is_within_tracked_surface(self.document.as_ref(), target)
            || self.is_within_shaded_element(target);
        Some(ShellHitSnapshot {
            node_id: target,
            over_tracked_surface,
        })
    }

    /// Verifies that the attachment is the shell tracked-surface element
    /// whose `src` names this tracked render-target index.
    pub fn validates_attachment(&self, attachment: usize, index: u32) -> bool {
        let dom_node = Some(NodeId::from_u64(attachment as u64));
        let element = dom_node
            .and_then(|node| self.document.as_ref().get_node(node))
            .and_then(blitz_dom::Node::element_data);
        let valid = element.is_some_and(|element| {
            is_tracked_surface_element(element)
                && element
                    .attr(local_name!("src"))
                    .and_then(|source| source.parse::<u32>().ok())
                    == Some(index)
        });
        if !valid {
            tracing::warn!(
                attachment,
                index,
                ?dom_node,
                tag = element.map(|element| &*element.name.local),
                source = element.and_then(|element| element.attr(local_name!("src"))),
                "polymer: invalid shell surface attachment"
            );
        }
        valid
    }

    /// The `<tracked-surface-group>` composing an attached tracked surface.
    pub fn attachment_surface_group(&self, attachment: usize) -> Option<u64> {
        enclosing_surface_group(self.document.as_ref(), NodeId::from_u64(attachment as u64))
            .map(SurfaceGroupBounds::id_for)
    }

    fn resolve_document(&mut self, invalidate_layout: bool) {
        if invalidate_layout {
            // Blitz intrinsic measurement mutates Parley's line layout. A cached
            // final Taffy box can then skip restoring it, painting min-content
            // lines inside a correctly sized box. Invalidate measurement caches
            // before geometry-changing resolves, including newly loaded fonts.
            // Keep Stylo's incremental restyling and paint-only/scroll animation
            // paths intact; this adds no extra resolve or delayed repaint.
            let ids: Vec<_> = self
                .document
                .as_ref()
                .tree()
                .iter()
                .map(|(id, _)| id)
                .collect();
            for id in ids {
                self.document
                    .as_mut()
                    .get_node_mut(id)
                    .unwrap()
                    .clear_layout_cache();
            }
        }
        let animation_time = self.animation_time();
        self.document.as_mut().resolve(animation_time);
    }

    /// Re-lays out and publishes paint scenes when the shell changed.
    pub fn produce_frame(&mut self, runtime: &mut ShellRuntime) -> Option<ShellFrame> {
        let assets_arrived = self.take_arrivals();
        let levels_arrived = self.images.begin_frame(f64::from(self.scale_factor));
        let animating = self.document.as_ref().is_animating();
        self.scroll_animating &= animating;
        if !assets_arrived && !levels_arrived && !self.dirty && !self.animation_frame_due() {
            return None;
        }
        self.last_animation_frame = std::time::Instant::now();
        self.dirty = false;
        // Resolving is what issues the fetches, and this provider serves them
        // before returning, so anything the document asked for is waiting by
        // the time it returns. Resolving again until nothing new arrives means
        // an asset that only became reachable through another one still makes
        // this paint, rather than waiting for an unrelated event to trigger the
        // next. The bound is there because a document that somehow kept asking
        // for more must not spin the shell.
        if assets_arrived || self.scroll_animating {
            self.needs_geometry_sync = true;
        }
        if assets_arrived || self.needs_resolve || animating {
            let invalidate_layout =
                assets_arrived || (self.needs_resolve && self.needs_geometry_sync);
            self.needs_resolve = false;
            let resolve_started = std::time::Instant::now();
            self.resolve_document(invalidate_layout);
            self.apply_pending_scrolls();
            for _ in 0..ASSET_ROUNDS {
                if !self.take_arrivals() {
                    break;
                }
                // Loaded images can change intrinsic dimensions even when CSS fixes their box.
                self.needs_geometry_sync = true;
                self.resolve_document(true);
            }
            let resolved = resolve_started.elapsed();
            let sync_started = std::time::Instant::now();
            if std::mem::take(&mut self.needs_geometry_sync) {
                let (logical_width, logical_height) = self.logical_size();
                runtime.stage_layout(
                    self.document.as_ref(),
                    logical_width,
                    logical_height,
                    self.scale_factor,
                );
            }
            if std::env::var_os(PAINT_TIMING).is_some() {
                tracing::error!(
                    "polymer: shell resolve {:.2}ms sync {:.2}ms",
                    resolved.as_secs_f64() * 1000.0,
                    sync_started.elapsed().as_secs_f64() * 1000.0,
                );
            }
        }
        // Blitz counts every custom widget as permanently animating. A tracked
        // surface is a retained compositor placeholder, not animated shell
        // chrome, so `is_animating()` would re-run Stylo and repaint the whole
        // document forever. Script mutations and native UI events explicitly
        // invalidate the shell when its chrome actually changes.

        let started = std::time::Instant::now();
        let (logical_width, logical_height) = self.logical_size();
        let (used, output) = {
            let mut scene =
                BandedScene::new(&mut self.bands, &mut self.placements, &mut self.images);
            // blitz-paint 0.3 deliberately leaves scene lifetime to its
            // caller. Reusing our retained Vello scenes without resetting
            // them accumulates every prior drawer position, border and shadow
            // across resize/drag animation frames.
            scene.reset();
            blitz_paint::paint_scene(
                &mut scene,
                self.document.as_mut(),
                1.0,
                logical_width,
                logical_height,
                0,
                0,
            );
            let used = scene.bands_used();
            let output = scene.take_compositing_output();
            (used, output)
        };
        apply_tracked_surface_styles(self.document.as_ref(), &mut self.placements);
        let encoded = started.elapsed();
        if std::env::var_os(PAINT_TIMING).is_some() {
            tracing::error!(
                "polymer: game layer encoded {}x{} in {:.2}ms over {used} band(s)",
                self.width,
                self.height,
                encoded.as_secs_f64() * 1000.0,
            );
        }
        // Flatten the exact paint sequence into independent chrome visuals and
        // tracked surfaces. Fragments from the same original band no longer
        // share a full-window texture merely because no guest surface happened
        // to separate them.
        let mut raster_chunks = Vec::new();
        let mut surface_layout = Vec::new();
        let mut z_order = 0i32;
        let mut group_first_z = HashMap::<u64, i32>::new();
        let mut chunk_ordinals = HashMap::<Option<u64>, u32>::new();
        let mut fragments = output.fragments.into_iter().peekable();
        let group_tree = output
            .groups
            .iter()
            .map(|group| (group.id, (group.parent, group.bounds)))
            .collect::<HashMap<_, _>>();
        for band in 0..used {
            while fragments
                .peek()
                .is_some_and(|fragment| fragment.band == band)
            {
                let fragment = fragments.next().expect("peeked fragment exists");
                let scale = f64::from(self.scale_factor);
                let viewport =
                    kurbo::Rect::new(0.0, 0.0, f64::from(self.width), f64::from(self.height));
                let bounds = chrome_fragment_bounds(
                    fragment.bounds.map(|bounds| scaled_rect(bounds, scale)),
                    viewport,
                );
                let mut local_scene = anyrender::Scene::with_tolerance(fragment.scene.tolerance);
                local_scene.append_scene(
                    fragment.scene,
                    kurbo::Affine::new([scale, 0.0, 0.0, scale, -bounds.x0, -bounds.y0]),
                );
                if let Some(group) = fragment.group {
                    group_first_z.entry(group).or_insert(z_order);
                }
                let ordinal = chunk_ordinals.entry(fragment.group).or_default();
                let id = RasterChunkId {
                    owner: fragment.group,
                    ordinal: *ordinal,
                };
                *ordinal += 1;
                raster_chunks.push(ShellRasterChunk {
                    id,
                    scene: local_scene,
                    bounds,
                    z_order,
                    group: fragment.group,
                });
                z_order += 1;
            }
            for placement in self
                .placements
                .iter()
                .filter(|placement| placement.band == band)
            {
                let Some(node_id) = placement.node_id else {
                    continue;
                };
                let node_id = node_id.as_u64() as usize;
                surface_layout.push(TrackedSurfaceLayout {
                    node_id,
                    source: TrackedSource::from_id(placement.source_id),
                    bounds: scaled_rect(placement.rect, f64::from(self.scale_factor)),
                    z_order,
                    group: placement.group,
                    corner_radius: placement.corner_radius * self.scale_factor,
                    style: placement.style.clone(),
                    opacity: placement.opacity,
                    surface_group: placement.surface_group.map(|mut surface_group| {
                        let inner = surface_group.bounds.inflate(-0.5, -0.5);
                        surface_group.parent =
                            std::iter::successors(surface_group.parent, |group| {
                                group_tree.get(group).and_then(|(parent, _)| *parent)
                            })
                            .find(|group| {
                                group_tree
                                    .get(group)
                                    .is_some_and(|(_, bounds)| bounds.union(inner) == *bounds)
                            });
                        surface_group.bounds =
                            scaled_rect(surface_group.bounds, f64::from(self.scale_factor));
                        surface_group
                    }),
                });
                if let Some(group) = placement.group {
                    group_first_z.entry(group).or_insert(z_order);
                }
                z_order += 1;
            }
        }
        let composition_groups = output
            .groups
            .into_iter()
            .map(|group| ShellCompositionGroup {
                id: group.id,
                parent: group.parent,
                bounds: scaled_rect(group.bounds, f64::from(self.scale_factor)),
                z_order: group_first_z.get(&group.id).copied().unwrap_or(i32::MAX),
                opacity: group.opacity,
                shadow: group.shadow.map(|mut shadow| {
                    shadow.offset[0] *= self.scale_factor;
                    shadow.offset[1] *= self.scale_factor;
                    shadow.blur_radius *= self.scale_factor;
                    shadow
                }),
                backdrop_blur: group
                    .backdrop_blur
                    .filter(|_| group_first_z.contains_key(&group.id))
                    .map(|blur| blur * self.scale_factor),
            })
            .collect();
        Some(ShellFrame {
            raster_chunks,
            composition_groups,
            surface_layout,
            width: self.width,
            height: self.height,
            mip_generation: polymer_mipmap::MipImages::generation(),
            structure_revision: self.dom_structure_revision,
        })
    }
}

impl ShellDocument {
    /// Hands the document every asset that has finished loading, reporting
    /// whether any did.
    fn take_arrivals(&mut self) -> bool {
        let mut arrived = false;
        while self.arrivals.try_recv().is_ok() {
            arrived = true;
        }
        arrived
    }
}
