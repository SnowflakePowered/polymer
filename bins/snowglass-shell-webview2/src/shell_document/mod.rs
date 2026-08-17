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

use crate::shell_dom::{ShellFrame, ShellHitSnapshot};
use crate::shell_runtime::ShellRuntime;
use anyrender::PaintScene as _;
use anyrender_vello::VelloScenePainter;
use bands::BandedScene;
pub use bands::{GamePlacement, GameViewStyle};
use blitz_dom::local_name;
use blitz_traits::net::{NetProvider, NetWaker};
pub use events::{DomKeyboardEvent, DomMouseEvent, DomWheelEvent};
use hit_test::{node_is_within_tracked_surface, pointer_hit};
#[cfg(test)]
use mutations::style_change_requires_geometry;
use mutations::{ContainedDamage, apply_dom_mutations, visual_damage_rect};
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
use vello::Scene;

/// How many times a paint will re-resolve to pick up assets that only became
/// reachable through an asset it just loaded.
const ASSET_ROUNDS: usize = 4;

/// Set to report what each repaint of this layer costs.
const PAINT_TIMING: &str = "SNOWGLASS_GAME_LAYER_TIMING";

pub struct ShellDocument {
    document: blitz_html::HtmlDocument,
    /// Assets arrive here because a fetch is issued while the document is
    /// mid-resolve and cannot take them straight away.
    arrivals: Receiver<()>,
    /// One encoded scene per band. Encoding is cheap and happens on the CPU;
    /// only rasterising is worth a GPU.
    bands: Vec<Scene>,
    placements: Vec<GamePlacement>,
    width: u32,
    height: u32,
    /// Set whenever layout or content changed, so a paint is only spent when
    /// something can differ. Guest frames arriving do not dirty this layer.
    dirty: bool,
    /// Damage accumulated for mutations below a CSS layout/paint containment
    /// boundary. Any uncontained change sets `full_repaint` and discards it.
    partial_damage: Option<kurbo::Rect>,
    /// Containment boundaries whose old bounds were captured before a pending
    /// style resolve. Their new bounds complete the damage union afterwards.
    contained_damage: Vec<ContainedDamage>,
    full_repaint: bool,
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
    script_to_dom: HashMap<usize, usize>,
    dom_to_script: HashMap<usize, usize>,
    /// Authoritative paint-order hover path. Blitz's built-in hit tester does
    /// not account for every hoisted overlay that its painter supports, so
    /// shell pointer routing and CSS hover must share this path.
    pointer_hover_path: Vec<usize>,
}

/// Factory for a shell document and its native network/window providers.
pub struct ShellDocumentFactory {
    event_proxy: crate::window_manager::EventLoopProxy<crate::app::UserEvent>,
}

impl ShellDocumentFactory {
    pub fn new(event_proxy: crate::window_manager::EventLoopProxy<crate::app::UserEvent>) -> Self {
        Self { event_proxy }
    }

    /// `views` is how many game views the document can show; it fixes the
    /// number of chrome bands, which is one more than that. The document names
    /// each one by its `<tracked-surface src>`, so this is a capacity, not a
    /// mapping.
    pub fn create(
        &self,
        html: &str,
        base_url: String,
        views: usize,
        width: u32,
        height: u32,
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
            .map(|(id, _)| (id, id))
            .collect::<HashMap<_, _>>();
        let dom_to_script = script_to_dom.clone();
        let mut document = ShellDocument {
            document,
            arrivals,
            bands: (0..=views).map(|_| Scene::new()).collect(),
            placements: Vec::new(),
            width: 0,
            height: 0,
            dirty: true,
            partial_damage: None,
            contained_damage: Vec::new(),
            full_repaint: true,
            needs_resolve: true,
            needs_geometry_sync: true,
            animation_epoch: std::time::Instant::now(),
            script_to_dom,
            dom_to_script,
            pointer_hover_path: Vec::new(),
        };
        document.resize(width, height);
        // Scripts run after parsing, with a complete DOM just like deferred
        // document scripts. Resolve once first so geometry APIs have useful
        // values during initialisation.
        document.document.as_mut().resolve(0.0);
        document
    }
}

impl ShellDocument {
    pub(crate) fn runtime_context(&self) -> (&blitz_dom::BaseDocument, u32, u32) {
        (self.document.as_ref(), self.width, self.height)
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        let (width, height) = (width.max(1), height.max(1));
        if (self.width, self.height) == (width, height) {
            return;
        }
        self.width = width;
        self.height = height;
        // Absolutely positioned boxes resolve against the viewport, so without
        // one every box in this layer lays out at zero size.
        self.document
            .as_mut()
            .set_viewport(blitz_traits::shell::Viewport::new(
                width,
                height,
                1.0,
                blitz_traits::shell::ColorScheme::Dark,
            ));
        self.dirty = true;
        self.full_repaint = true;
        self.partial_damage = None;
        self.contained_damage.clear();
        self.needs_resolve = true;
        self.needs_geometry_sync = true;
    }

    pub fn placements(&self) -> &[GamePlacement] {
        &self.placements
    }

    /// Forces the next paint even when the document itself did not change.
    ///
    /// This is also needed after the merged compositor moves to another GPU,
    /// because the replacement device must create fresh local chrome textures.
    pub fn invalidate(&mut self) {
        self.dirty = true;
        self.full_repaint = true;
        self.partial_damage = None;
        self.contained_damage.clear();
    }

    pub fn needs_paint(&self) -> bool {
        self.dirty
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
                "snowglass: invalid shell surface attachment"
            );
        }
        valid
    }

    /// Re-lays out and publishes paint scenes when the shell changed.
    pub fn produce_frame(&mut self, runtime: &mut ShellRuntime) -> Option<ShellFrame> {
        let assets_arrived = self.take_arrivals();
        if !assets_arrived && !self.dirty {
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
            self.full_repaint = true;
            self.partial_damage = None;
            self.contained_damage.clear();
        }
        if assets_arrived || self.needs_resolve {
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
                let geometry_roots = (!self.full_repaint)
                    .then(|| {
                        self.contained_damage
                            .iter()
                            .map(|damage| damage.node)
                            .collect::<Vec<_>>()
                    })
                    .filter(|roots| !roots.is_empty());
                runtime.sync(
                    self.document.as_ref(),
                    self.width,
                    self.height,
                    &self.dom_to_script,
                    geometry_roots.as_deref(),
                );
            }
            if std::env::var_os(PAINT_TIMING).is_some() {
                tracing::error!(
                    "snowglass: shell resolve {:.2}ms sync {:.2}ms",
                    resolved.as_secs_f64() * 1000.0,
                    sync_started.elapsed().as_secs_f64() * 1000.0,
                );
            }
            if !self.full_repaint {
                for contained in self.contained_damage.drain(..) {
                    let after = visual_damage_rect(self.document.as_ref(), contained.node);
                    let damage = contained.before.union(after);
                    self.partial_damage = Some(
                        self.partial_damage
                            .map_or(damage, |current| current.union(damage)),
                    );
                }
            } else {
                self.contained_damage.clear();
            }
        }
        // Blitz counts every custom widget as permanently animating. A tracked
        // surface is a retained compositor placeholder, not animated shell
        // chrome, so `is_animating()` would re-run Stylo and repaint the whole
        // document forever. Script mutations and native UI events explicitly
        // invalidate the shell when its chrome actually changes.

        let started = std::time::Instant::now();
        let (used, overflowed) = {
            // The painters are a view onto the scenes, so they are built for
            // this pass and dropped before the scenes are rasterised.
            let mut painters = self
                .bands
                .iter_mut()
                .map(VelloScenePainter::new)
                .collect::<Vec<_>>();
            let mut scene = BandedScene::new(&mut painters, &mut self.placements);
            // blitz-paint 0.3 deliberately leaves scene lifetime to its
            // caller. Reusing our retained Vello scenes without resetting
            // them accumulates every prior drawer position, border and shadow
            // across resize/drag animation frames.
            scene.reset();
            blitz_paint::paint_scene(
                &mut scene,
                self.document.as_mut(),
                1.0,
                self.width,
                self.height,
                0,
                0,
            );
            (scene.bands_used(), scene.overflowed())
        };
        apply_tracked_surface_styles(self.document.as_ref(), &mut self.placements);
        if overflowed {
            tracing::error!(
                "snowglass: the game layer shows more than {} views, so the ones past that \
                 cannot be stacked against the chrome between them",
                self.bands.len() - 1
            );
        }
        let encoded = started.elapsed();
        if std::env::var_os(PAINT_TIMING).is_some() {
            tracing::error!(
                "snowglass: game layer encoded {}x{} in {:.2}ms over {used} band(s)",
                self.width,
                self.height,
                encoded.as_secs_f64() * 1000.0,
            );
        }
        let damage = (!self.full_repaint)
            .then_some(self.partial_damage)
            .flatten();
        self.full_repaint = false;
        self.partial_damage = None;
        Some(ShellFrame {
            chrome_bands: self.bands[..used].to_vec(),
            width: self.width,
            height: self.height,
            damage,
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
        let effect = apply_dom_mutations(
            &mut self.document,
            &mut self.script_to_dom,
            &mut self.dom_to_script,
            mutations,
        );
        if !effect.needs_resolve {
            return;
        }
        self.dirty = true;
        self.needs_resolve |= effect.needs_resolve;
        self.needs_geometry_sync |= effect.needs_geometry_sync;
        if effect.unbounded {
            self.full_repaint = true;
            self.partial_damage = None;
            self.contained_damage.clear();
        } else if !self.full_repaint {
            self.contained_damage.extend(effect.contained);
        }
    }
}

#[cfg(test)]
mod tests;
