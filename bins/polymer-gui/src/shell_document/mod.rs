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
mod mutations;
mod providers;
mod tracked_surface;

use crate::shell_dom::{
    RasterChunkId, ShellCompositionGroup, ShellFrame, ShellHitSnapshot, ShellRasterChunk,
    TrackedSurfaceLayout,
};
use crate::shell_runtime::{Mutation, ShellRuntime};
use anyrender::PaintScene as _;
use bands::BandedScene;
pub use bands::{CompositionShadowStyle, GamePlacement, GameViewStyle};
use blitz_dom::NodeId;
use blitz_dom::local_name;
use blitz_traits::net::{NetProvider, NetWaker};
pub use events::{DomKeyboardEvent, DomMouseEvent, DomWheelEvent};
use hit_test::{node_is_within_tracked_surface, pointer_hit};
use mutations::apply_dom_mutations;
use providers::NativeShellProvider;
use std::{
    collections::HashMap,
    sync::{
        Arc,
        mpsc::{Receiver, channel},
    },
};
use tracked_surface::{
    apply_tracked_surface_styles, initialise_tracked_surfaces, is_tracked_surface_element,
};

/// How many times a paint will re-resolve to pick up assets that only became
/// reachable through an asset it just loaded.
const ASSET_ROUNDS: usize = 4;

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
    /// but their synchronous DOM geometry is derived from the mirrored style.
    needs_geometry_sync: bool,
    animation_epoch: std::time::Instant,
    /// Solid-created nodes use monotonically increasing JavaScript IDs while
    /// Blitz allocates its own arena IDs. Initial parsed nodes are identity
    /// mapped; these maps keep later mutations and hit testing in agreement.
    script_to_dom: HashMap<usize, NodeId>,
    dom_to_script: HashMap<NodeId, usize>,
    /// Paint-order hover path used only while Blitz's native hit target
    /// differs from the compositor-visible target.
    pointer_hover_path: Vec<NodeId>,
}

/// Factory for a shell document and its native network/window providers.
pub struct ShellDocumentFactory {
    event_proxy: crate::window_manager::EventLoopProxy<crate::app::UserEvent>,
}

impl ShellDocumentFactory {
    pub fn new(event_proxy: crate::window_manager::EventLoopProxy<crate::app::UserEvent>) -> Self {
        Self { event_proxy }
    }

    pub fn create(
        &self,
        html: &str,
        base_url: String,
        width: u32,
        height: u32,
        scale_factor: f32,
    ) -> ShellDocument {
        let (sender, arrivals) = channel();
        let proxy = self.event_proxy.clone();
        let waker: Arc<dyn NetWaker> = Arc::new(move |_| {
            let _ = sender.send(());
            let _ = proxy.send_event(crate::commands::UserEvent::Window(
                crate::window_manager::WindowCommand::RedrawShell,
            ));
        });
        let net: Arc<dyn NetProvider> = blitz_net::Provider::shared(Some(waker));
        let shell_provider: Arc<dyn blitz_traits::shell::ShellProvider> =
            Arc::new(NativeShellProvider::new(self.event_proxy.clone()));
        let mut document = blitz_html::HtmlDocument::from_html(
            html,
            blitz_dom::DocumentConfig {
                base_url: Some(base_url.clone()),
                net_provider: Some(net),
                shell_provider: Some(shell_provider),
                ..Default::default()
            },
        );
        initialise_tracked_surfaces(&mut document);
        let script_to_dom = document
            .as_ref()
            .tree()
            .iter()
            .enumerate()
            .map(|(index, (id, _))| (index + 1, id))
            .collect::<HashMap<_, _>>();
        let dom_to_script = script_to_dom
            .iter()
            .map(|(script, dom)| (*dom, *script))
            .collect();
        let mut document = ShellDocument {
            document,
            dom_structure_revision: 1,
            arrivals,
            bands: vec![anyrender::Scene::new()],
            placements: Vec::new(),
            width: 0,
            height: 0,
            scale_factor: 1.0,
            dirty: true,
            needs_resolve: true,
            needs_geometry_sync: true,
            animation_epoch: std::time::Instant::now(),
            script_to_dom,
            dom_to_script,
            pointer_hover_path: Vec::new(),
        };
        document.resize(width, height, scale_factor);
        // Scripts run after parsing, with a complete DOM just like deferred
        // document scripts. Resolve once first so geometry APIs have useful
        // values during initialisation.
        document.document.as_mut().resolve(0.0);
        document
    }
}

impl ShellDocument {
    pub(crate) fn runtime_context(&self) -> (&blitz_dom::BaseDocument, u32, u32, f32) {
        let (width, height) = self.logical_size();
        (self.document.as_ref(), width, height, self.scale_factor)
    }

    pub(crate) fn runtime_node_ids(&self) -> &HashMap<NodeId, usize> {
        &self.dom_to_script
    }

    pub(crate) const fn dom_structure_revision(&self) -> u64 {
        self.dom_structure_revision
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

    pub fn resize(&mut self, width: u32, height: u32, scale_factor: f32) {
        let (width, height) = (width.max(1), height.max(1));
        let scale_factor = normalized_scale_factor(scale_factor);
        if (self.width, self.height, self.scale_factor) == (width, height, scale_factor) {
            return;
        }
        self.width = width;
        self.height = height;
        self.scale_factor = scale_factor;
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
        self.dirty || self.document.as_ref().is_animating()
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
        .find(|node| self.dom_to_script.contains_key(node))?;
        let over_tracked_surface = node_is_within_tracked_surface(self.document.as_ref(), target);
        Some(ShellHitSnapshot {
            node_id: target,
            over_tracked_surface,
        })
    }

    /// Verifies that the attachment is the shell tracked-surface element
    /// whose `src` names this tracked render-target index.
    pub fn validates_attachment(&self, attachment: usize, index: u32) -> bool {
        let dom_node = self.script_to_dom.get(&attachment).copied();
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
                tag = element.map(|element| element.name.local.as_ref()),
                source = element.and_then(|element| element.attr(local_name!("src"))),
                "polymer: invalid shell surface attachment"
            );
        }
        valid
    }

    /// Re-lays out and publishes paint scenes when the shell changed.
    pub fn produce_frame(&mut self, runtime: &mut ShellRuntime) -> Option<ShellFrame> {
        let assets_arrived = self.take_arrivals();
        let animating = self.document.as_ref().is_animating();
        if !assets_arrived && !self.dirty && !animating {
            return None;
        }
        self.dirty = false;
        // Resolving is what issues the fetches, and this provider serves them
        // before returning, so anything the document asked for is waiting by
        // the time it returns. Resolving again until nothing new arrives means
        // an asset that only became reachable through another one still makes
        // this paint, rather than waiting for an unrelated event to trigger the
        // next. The bound is there because a document that somehow kept asking
        // for more must not spin the shell.
        if assets_arrived {
            self.needs_geometry_sync = true;
        }
        if assets_arrived || self.needs_resolve || animating {
            self.needs_resolve = false;
            let resolve_started = std::time::Instant::now();
            let animation_time = self.animation_time();
            self.document.as_mut().resolve(animation_time);
            for _ in 0..ASSET_ROUNDS {
                if !self.take_arrivals() {
                    break;
                }
                let animation_time = self.animation_time();
                self.document.as_mut().resolve(animation_time);
            }
            let resolved = resolve_started.elapsed();
            let sync_started = std::time::Instant::now();
            if std::mem::take(&mut self.needs_geometry_sync) {
                let (logical_width, logical_height) = self.logical_size();
                runtime.sync(
                    self.document.as_ref(),
                    logical_width,
                    logical_height,
                    self.scale_factor,
                    &self.dom_to_script,
                    None,
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
            let mut scene = BandedScene::new(&mut self.bands, &mut self.placements);
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
                let Some(node_id) = self.dom_to_script.get(&node_id).copied() else {
                    continue;
                };
                surface_layout.push(TrackedSurfaceLayout {
                    node_id,
                    source_id: placement.source_id,
                    bounds: scaled_rect(placement.rect, f64::from(self.scale_factor)),
                    z_order,
                    group: placement.group,
                    corner_radius: placement.corner_radius * self.scale_factor,
                    style: placement.style.clone(),
                    opacity: placement.opacity,
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
            })
            .collect();
        Some(ShellFrame {
            raster_chunks,
            composition_groups,
            surface_layout,
            width: self.width,
            height: self.height,
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

    pub(crate) fn apply_script_mutations(&mut self, runtime: &mut ShellRuntime) {
        let mutations = runtime.take_mutations();
        if mutations.is_empty() {
            return;
        }
        let structure_changed = mutations.iter().any(|mutation| {
            matches!(
                mutation,
                Mutation::AppendChild { .. }
                    | Mutation::InsertBefore { .. }
                    | Mutation::RemoveChild { .. }
            )
        });
        let effect = apply_dom_mutations(
            &mut self.document,
            &mut self.script_to_dom,
            &mut self.dom_to_script,
            mutations,
        );
        if structure_changed {
            self.dom_structure_revision = self.dom_structure_revision.wrapping_add(1);
        }
        if !effect.needs_resolve {
            return;
        }
        self.dirty = true;
        self.needs_resolve |= effect.needs_resolve;
        self.needs_geometry_sync |= effect.needs_geometry_sync;
    }
}

#[cfg(test)]
mod tests;
