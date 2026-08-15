//! Lays out and paints the background document with Blitz.
//!
//! Everything between the background and the foreground WebView2 lives here:
//! the backdrop, the bezel art, and the boxes the guest frames are shown in.
//! The foreground stays in WebView2 because it needs the whole web platform;
//! this layer needs CSS layout and nothing else, so it can be driven by a
//! renderer we control and composited without a browser in the path.
//!
//! A game view is written as `<tracked-surface src="N">`, where N is the
//! tracked render target. Painting diverts those into rectangles
//! ([`bands::GamePlacement`])
//! and splits the remaining chrome into bands, which the compositor stacks
//! around them.

mod bands;

use crate::background_dom::{BackgroundFrame, BackgroundHitSnapshot};
use crate::background_runtime::{
    BackgroundRuntime, EmulatorCommandSink, KeyboardEvent, MouseEvent, Mutation,
};
use anyrender::PaintScene as _;
use anyrender_vello::VelloScenePainter;
use bands::BandedScene;
pub use bands::{GamePlacement, GameViewStyle};
use blitz_dom::node::{Node, Widget};
use blitz_dom::{Document as _, LocalName, QualName, local_name, ns};
use blitz_traits::events::{
    BlitzPointerEvent, BlitzPointerId, MouseEventButton, MouseEventButtons, Point, PointerCoords,
    PointerDetails, UiEvent,
};
use blitz_traits::net::{NetHandler, NetProvider, Request};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        mpsc::{Receiver, Sender, channel},
    },
};
use style::values::computed::PointerEvents;
use vello::Scene;

/// How many times a paint will re-resolve to pick up assets that only became
/// reachable through an asset it just loaded.
const ASSET_ROUNDS: usize = 4;

/// Set to report what each repaint of this layer costs.
const PAINT_TIMING: &str = "SNOWGLASS_GAME_LAYER_TIMING";

/// Hit-tests in the same stacking order Blitz paints while allowing a child
/// to restore `pointer-events: auto` beneath a transparent parent.
///
/// Blitz 0.3 hoists non-zero z-index descendants out of `paint_children`.
/// Those lists therefore have to be traversed explicitly around the ordinary
/// children, just as the painter does.
fn pointer_hit(document: &blitz_dom::BaseDocument, x: f32, y: f32) -> Option<usize> {
    pointer_hit_node(document, document.root_element(), x, y)
}

fn pointer_hit_node(
    document: &blitz_dom::BaseDocument,
    node: &Node,
    x: f32,
    y: f32,
) -> Option<usize> {
    let mut local_x = x - node.final_layout.location.x + node.scroll_offset.x as f32;
    let mut local_y = y - node.final_layout.location.y + node.scroll_offset.y as f32;
    if let Some(transform) = node.transform {
        let point = transform.inverse() * kurbo::Point::new(local_x.into(), local_y.into());
        local_x = point.x as f32;
        local_y = point.y as f32;
    }

    let size = node.final_layout.size;
    let matches_self = local_x >= 0.0
        && local_x <= size.width + node.scroll_offset.x as f32
        && local_y >= 0.0
        && local_y <= size.height + node.scroll_offset.y as f32;
    let content = node.final_layout.content_size;
    let matches_content = local_x >= 0.0
        && local_x <= content.width + node.scroll_offset.x as f32
        && local_y >= 0.0
        && local_y <= content.height + node.scroll_offset.y as f32;
    let overflow = node.scrollable_overflow;
    let matches_overflow = local_x >= overflow.x0 as f32
        && local_x <= overflow.x1 as f32
        && local_y >= overflow.y0 as f32
        && local_y <= overflow.y1 as f32;
    let matches_hoisted = node.stacking_context.as_ref().is_some_and(|context| {
        let area = context.content_area;
        local_x >= area.left + node.scroll_offset.x as f32
            && local_x <= area.right + node.scroll_offset.x as f32
            && local_y >= area.top + node.scroll_offset.y as f32
            && local_y <= area.bottom + node.scroll_offset.y as f32
    });
    if !matches_self && !matches_content && !matches_overflow && !matches_hoisted {
        return None;
    }

    if node.flags.is_inline_root() {
        local_x -= node.final_layout.padding.left + node.final_layout.border.left;
        local_y -= node.final_layout.padding.top + node.final_layout.border.top;
    }

    if let Some(context) = &node.stacking_context {
        for child in context.pos_z_hoisted_children().rev() {
            if let Some(hit) = document.get_node(child.node_id).and_then(|node| {
                pointer_hit_node(
                    document,
                    node,
                    local_x - child.position.x,
                    local_y - child.position.y,
                )
            }) {
                return Some(hit);
            }
        }
    }

    let children = node.paint_children.borrow().clone().unwrap_or_default();
    for child in children.into_iter().rev() {
        if let Some(hit) = document
            .get_node(child)
            .and_then(|child| pointer_hit_node(document, child, local_x, local_y))
        {
            return Some(hit);
        }
    }

    if let Some(context) = &node.stacking_context {
        for child in context.neg_z_hoisted_children().rev() {
            if let Some(hit) = document.get_node(child.node_id).and_then(|node| {
                pointer_hit_node(
                    document,
                    node,
                    local_x - child.position.x,
                    local_y - child.position.y,
                )
            }) {
                return Some(hit);
            }
        }
    }

    let accepts_pointer = node
        .primary_styles()
        .is_none_or(|styles| styles.clone_pointer_events() != PointerEvents::None);
    (matches_self && accepts_pointer).then_some(node.id)
}

/// Loads the layer's own assets, and only those.
///
/// A linked document can use the private `snowglass:` scheme for assets
/// embedded in the executable, while a `file:` document may reference files
/// beside itself.
/// There is nothing to fetch over a network, so `blitz-net` would bring an
/// async runtime along to do less than this.
struct LocalAssets {
    arrivals: Sender<()>,
}

#[derive(Default)]
struct TrackedSurfaceWidget {
    source_id: Option<u64>,
}

impl TrackedSurfaceWidget {
    fn new(source_id: Option<u64>) -> Self {
        Self { source_id }
    }
}

impl Widget for TrackedSurfaceWidget {
    fn attribute_changed(&mut self, name: &str, _old_value: Option<&str>, new_value: Option<&str>) {
        if name == "src" {
            self.source_id = new_value.and_then(|value| value.parse().ok());
        }
    }

    fn paint(
        &mut self,
        _render_ctx: &mut dyn anyrender::RenderContext,
        _styles: &blitz_dom::node::ComputedStyles,
        width: u32,
        height: u32,
        _scale: f64,
    ) -> anyrender::Scene {
        let mut scene = anyrender::Scene::new();
        if let Some(source_id) = self.source_id {
            let paint = anyrender::Paint::Resource(peniko::ImageBrush {
                image: anyrender::ResourceId::from_ffi(source_id),
                sampler: Default::default(),
            });
            scene.fill(
                peniko::Fill::NonZero,
                kurbo::Affine::IDENTITY,
                &paint,
                None,
                &kurbo::Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
            );
        }
        scene
    }
}

impl NetProvider for LocalAssets {
    fn fetch(&self, _doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        let url = request.url;
        let bytes = match url.scheme() {
            "file" => {
                let Ok(path) = url.to_file_path() else {
                    tracing::error!("snowglass: the game layer cannot resolve {url} to a path");
                    return;
                };
                match std::fs::read(&path) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        tracing::error!(
                            "snowglass: the game layer could not read {}: {error}",
                            path.display()
                        );
                        return;
                    }
                }
            }
            _ => {
                tracing::error!("snowglass: the game layer will not fetch {url}");
                return;
            }
        };
        // Parsing the bytes and queueing the resulting document update is the
        // handler's job. The local signal lets this synchronous provider keep
        // resolving until newly discovered assets have also been requested.
        handler.bytes(url.to_string(), bytes.into());
        let _ = self.arrivals.send(());
    }
}

pub struct BackgroundDocument {
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
    /// React-created nodes use monotonically increasing JavaScript IDs while
    /// Blitz allocates its own arena IDs. Initial parsed nodes are identity
    /// mapped; these maps keep later mutations and hit testing in agreement.
    script_to_dom: HashMap<usize, usize>,
    dom_to_script: HashMap<usize, usize>,
    mouse_down: Option<usize>,
}

pub struct DomMouseEvent<'a> {
    pub kind: &'a str,
    pub x: f32,
    pub y: f32,
    pub button: u8,
    pub buttons: u8,
    pub ctrl_key: bool,
    pub shift_key: bool,
}

pub struct DomKeyboardEvent<'a> {
    pub kind: &'a str,
    pub key: String,
    pub code: String,
    pub repeat: bool,
    pub alt_key: bool,
    pub ctrl_key: bool,
    pub shift_key: bool,
}

/// GPU state prepared before WebView2 starts its renderer.
///
/// Creating Vello's D3D12 device and pipelines after Chromium has initialized
/// its WARP path can crash inside `d3d10warp.dll`. The foreground URL still
/// decides whether a layer document exists; this only reserves the renderer
/// that will paint it if one is discovered.
#[derive(Clone)]
pub struct BackgroundDocumentFactory {
    plugin_runtime: Arc<snowglass_pluginhost::PluginRuntime>,
    background_bus: Option<crate::shared_bus::BackgroundBusEndpoint>,
}

impl BackgroundDocumentFactory {
    pub fn new(plugin_registry: snowglass_pluginhost::PluginRegistry) -> Self {
        Self {
            plugin_runtime: Arc::new(snowglass_pluginhost::PluginRuntime::new(
                plugin_registry,
                snowglass_pluginhost::PluginRuntimeOptions::default(),
            )),
            background_bus: None,
        }
    }

    pub fn plugin_runtime(&self) -> Arc<snowglass_pluginhost::PluginRuntime> {
        self.plugin_runtime.clone()
    }

    pub fn attach_shared_bus(&mut self, endpoint: crate::shared_bus::BackgroundBusEndpoint) {
        self.background_bus = Some(endpoint);
    }

    /// `views` is how many game views the document can show; it fixes the
    /// number of chrome bands, which is one more than that. The document names
    /// each one by its `<tracked-surface src>`, so this is a capacity, not a
    /// mapping.
    pub async fn create(
        self,
        html: &str,
        base_url: String,
        views: usize,
        width: u32,
        height: u32,
        emulator_commands: EmulatorCommandSink,
    ) -> (BackgroundDocument, BackgroundRuntime) {
        let (sender, arrivals) = channel();
        let net = Arc::new(LocalAssets { arrivals: sender });
        let mut document = blitz_html::HtmlDocument::from_html(
            html,
            blitz_dom::DocumentConfig {
                base_url: Some(base_url.clone()),
                net_provider: Some(net),
                ..Default::default()
            },
        );
        initialise_tracked_surfaces(&mut document);
        self.build(
            document,
            arrivals,
            html,
            &base_url,
            views,
            (width, height),
            emulator_commands,
        )
        .await
    }
}

impl BackgroundDocumentFactory {
    async fn build(
        self,
        document: blitz_html::HtmlDocument,
        arrivals: Receiver<()>,
        html: &str,
        source_name: &str,
        views: usize,
        viewport: (u32, u32),
        emulator_commands: EmulatorCommandSink,
    ) -> (BackgroundDocument, BackgroundRuntime) {
        let (width, height) = viewport;
        let script_to_dom = document
            .as_ref()
            .tree()
            .iter()
            .map(|(id, _)| (id, id))
            .collect::<HashMap<_, _>>();
        let dom_to_script = script_to_dom.clone();
        let background_bus = self.background_bus;
        let mut layer = BackgroundDocument {
            document,
            arrivals,
            bands: (0..=views).map(|_| Scene::new()).collect(),
            placements: Vec::new(),
            width: 0,
            height: 0,
            dirty: true,
            script_to_dom,
            dom_to_script,
            mouse_down: None,
        };
        layer.resize(width, height);
        // Scripts run after parsing, with a complete DOM just like deferred
        // document scripts. Resolve once first so geometry APIs have useful
        // values during initialisation.
        layer.document.as_mut().resolve(1.0);
        let mut runtime = BackgroundRuntime::start(
            layer.document.as_ref(),
            layer.width,
            layer.height,
            html.to_owned(),
            source_name.to_owned(),
            emulator_commands,
            background_bus,
        )
        .await;
        layer.apply_script_mutations(&mut runtime);
        (layer, runtime)
    }
}

impl BackgroundDocument {
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
    }

    pub fn needs_paint(&self) -> bool {
        self.dirty
    }

    pub fn hit_test(&self, x: f32, y: f32) -> Option<BackgroundHitSnapshot> {
        let target = pointer_hit(self.document.as_ref(), x, y)?;
        let over_tracked_surface = std::iter::successors(Some(target), |node| {
            self.document
                .as_ref()
                .get_node(*node)
                .and_then(|node| node.parent)
        })
        .any(|node| {
            self.document
                .as_ref()
                .get_node(node)
                .and_then(blitz_dom::Node::element_data)
                .is_some_and(is_tracked_surface_element)
        });
        Some(BackgroundHitSnapshot {
            node_id: target,
            over_tracked_surface,
        })
    }

    /// Dispatches a mouse event to a target selected from a background hit
    /// snapshot. The return value is true when script claims the pointer from
    /// the guest surface below this layer.
    pub fn mouse_event(
        &mut self,
        runtime: &mut BackgroundRuntime,
        hit: BackgroundHitSnapshot,
        event: DomMouseEvent<'_>,
    ) -> bool {
        let DomMouseEvent {
            kind,
            x,
            y,
            button,
            buttons,
            ctrl_key,
            shift_key,
        } = event;
        let target = hit.node_id;
        let native_event = MouseEvent {
            client_x: x,
            client_y: y,
            button,
            buttons,
            ctrl_key,
            shift_key,
            alt_key: false,
            meta_key: false,
        };
        let script_target = self.dom_to_script.get(&target).copied().unwrap_or(target);
        let result = runtime.dispatch_mouse(kind, script_target, native_event);
        let mut claimed = result.default_prevented || (result.handled && !hit.over_tracked_surface);
        if kind == "mousedown" {
            self.mouse_down = Some(target);
        } else if kind == "mouseup" {
            let clicked = button == 0 && self.mouse_down.take() == Some(target);
            if clicked {
                let click = runtime.dispatch_mouse(
                    "click",
                    script_target,
                    MouseEvent {
                        client_x: x,
                        client_y: y,
                        button,
                        buttons,
                        ctrl_key,
                        shift_key,
                        alt_key: false,
                        meta_key: false,
                    },
                );
                claimed |= click.default_prevented || (click.handled && !hit.over_tracked_surface);
            }
        }
        self.apply_script_mutations(runtime);
        if !result.default_prevented {
            let data = BlitzPointerEvent {
                id: BlitzPointerId::Mouse,
                is_primary: true,
                coords: PointerCoords {
                    page_x: x,
                    page_y: y,
                    screen_x: x,
                    screen_y: y,
                    client_x: x,
                    client_y: y,
                },
                button: match button {
                    1 => MouseEventButton::Auxiliary,
                    2 => MouseEventButton::Secondary,
                    3 => MouseEventButton::Fourth,
                    4 => MouseEventButton::Fifth,
                    _ => MouseEventButton::Main,
                },
                buttons: MouseEventButtons::from_bits_truncate(buttons),
                mods: Default::default(),
                details: PointerDetails::default(),
                element: Point::default(),
                active_pointers: Default::default(),
            };
            let event = match kind {
                "mousedown" => Some(UiEvent::PointerDown(data)),
                "mouseup" => Some(UiEvent::PointerUp(data)),
                "mousemove" => Some(UiEvent::PointerMove(data)),
                _ => None,
            };
            if let Some(event) = event {
                self.document.handle_ui_event(event);
            }
        }
        claimed
    }

    /// Dispatches a regular keyboard event to the background window.
    pub fn keyboard_event(
        &mut self,
        runtime: &mut BackgroundRuntime,
        event: DomKeyboardEvent<'_>,
    ) -> bool {
        let result = runtime.dispatch_keyboard(
            event.kind,
            KeyboardEvent {
                key: event.key,
                code: event.code,
                repeat: event.repeat,
                alt_key: event.alt_key,
                ctrl_key: event.ctrl_key,
                shift_key: event.shift_key,
                meta_key: false,
            },
        );
        let prevented = result.default_prevented;
        self.apply_script_mutations(runtime);
        prevented
    }

    /// Verifies that the attachment is the background tracked-surface element
    /// whose `src` names this tracked render-target index.
    pub fn validates_attachment(&self, attachment: usize, index: u32) -> bool {
        self.script_to_dom
            .get(&attachment)
            .copied()
            .and_then(|node| self.document.as_ref().get_node(node))
            .and_then(blitz_dom::Node::element_data)
            .is_some_and(|element| {
                is_tracked_surface_element(element)
                    && element
                        .attr(local_name!("src"))
                        .and_then(|source| source.parse::<u32>().ok())
                        == Some(index)
            })
    }

    /// Re-lays out and publishes paint scenes when the background changed.
    pub fn produce_frame(&mut self, runtime: &mut BackgroundRuntime) -> BackgroundFrame {
        if !self.take_arrivals() && !self.dirty {
            return BackgroundFrame {
                chrome_bands: Vec::new(),
                width: self.width,
                height: self.height,
                dirty: false,
            };
        }
        self.dirty = false;
        // Resolving is what issues the fetches, and this provider serves them
        // before returning, so anything the document asked for is waiting by
        // the time it returns. Resolving again until nothing new arrives means
        // an asset that only became reachable through another one still makes
        // this paint, rather than waiting for an unrelated event to trigger the
        // next. The bound is there because a document that somehow kept asking
        // for more must not spin the shell.
        self.document.as_mut().resolve(1.0);
        for _ in 0..ASSET_ROUNDS {
            if !self.take_arrivals() {
                break;
            }
            self.document.as_mut().resolve(1.0);
        }
        runtime.sync(
            self.document.as_ref(),
            self.width,
            self.height,
            &self.dom_to_script,
        );

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
        BackgroundFrame {
            chrome_bands: self.bands[..used].to_vec(),
            width: self.width,
            height: self.height,
            dirty: true,
        }
    }
}

impl BackgroundDocument {
    /// Hands the document every asset that has finished loading, reporting
    /// whether any did.
    fn take_arrivals(&mut self) -> bool {
        let mut arrived = false;
        while self.arrivals.try_recv().is_ok() {
            arrived = true;
        }
        arrived
    }

    pub(crate) fn apply_script_mutations(&mut self, runtime: &mut BackgroundRuntime) {
        let mutations = runtime.take_mutations();
        if apply_dom_mutations(
            &mut self.document,
            &mut self.script_to_dom,
            &mut self.dom_to_script,
            mutations,
        ) {
            self.dirty = true;
        }
    }
}

fn apply_dom_mutations(
    document: &mut blitz_html::HtmlDocument,
    script_to_dom: &mut HashMap<usize, usize>,
    dom_to_script: &mut HashMap<usize, usize>,
    mutations: Vec<Mutation>,
) -> bool {
    if mutations.is_empty() {
        return false;
    }

    // Stylo assumes every descendant reached by attribute invalidation already
    // has a primary computed style. React can append a newly-created subtree
    // and then set an attribute on one of its ancestors in the same mutation
    // batch. If we feed that order directly to Blitz, the later invalidation
    // sees the attached-but-unstyled descendants and panics in
    // `ElementStyles::primary`.
    //
    // Attach the new tree without attribute invalidations, give it its base
    // styles, then apply attributes. The next normal paint resolve computes
    // the final styles; the intermediate resolve is never presented.
    let has_new_elements = mutations
        .iter()
        .any(|mutation| matches!(mutation, Mutation::CreateElement { .. }));
    if has_new_elements {
        let (attributes, structure): (Vec<_>, Vec<_>) =
            mutations.into_iter().partition(|mutation| {
                matches!(
                    mutation,
                    Mutation::SetAttribute { .. } | Mutation::RemoveAttribute { .. }
                )
            });
        apply_dom_mutation_batch(document, script_to_dom, dom_to_script, structure);
        document.as_mut().resolve(1.0);
        apply_dom_mutation_batch(document, script_to_dom, dom_to_script, attributes);
    } else {
        apply_dom_mutation_batch(document, script_to_dom, dom_to_script, mutations);
    }
    true
}

fn apply_dom_mutation_batch(
    document: &mut blitz_html::HtmlDocument,
    script_to_dom: &mut HashMap<usize, usize>,
    dom_to_script: &mut HashMap<usize, usize>,
    mutations: Vec<Mutation>,
) {
    let mut mutator = document.as_mut().mutate();
    for mutation in mutations {
        match mutation {
            Mutation::CreateElement { node, tag } => {
                let is_tracked_surface = tag == "tracked-surface";
                let actual = mutator.create_element(
                    QualName::new(None, ns!(html), LocalName::from(tag)),
                    Vec::new(),
                );
                if is_tracked_surface {
                    mutator
                        .doc
                        .set_custom_widget(actual, Box::new(TrackedSurfaceWidget::default()));
                }
                script_to_dom.insert(node, actual);
                dom_to_script.insert(actual, node);
            }
            Mutation::CreateText { node, text } => {
                let actual = mutator.create_text_node(&text);
                script_to_dom.insert(node, actual);
                dom_to_script.insert(actual, node);
            }
            Mutation::CreateComment { node } => {
                let actual = mutator.create_comment_node();
                script_to_dom.insert(node, actual);
                dom_to_script.insert(actual, node);
            }
            Mutation::AppendChild { parent, child } => {
                let Some((parent, child)) = script_to_dom
                    .get(&parent)
                    .copied()
                    .zip(script_to_dom.get(&child).copied())
                else {
                    continue;
                };
                mutator.append_children(parent, &[child]);
            }
            Mutation::InsertBefore {
                parent,
                child,
                before,
            } => {
                let Some((parent, child, before)) = script_to_dom
                    .get(&parent)
                    .copied()
                    .zip(script_to_dom.get(&child).copied())
                    .zip(script_to_dom.get(&before).copied())
                    .map(|((parent, child), before)| (parent, child, before))
                else {
                    continue;
                };
                if mutator.parent_id(before) == Some(parent) {
                    mutator.insert_nodes_before(before, &[child]);
                }
            }
            Mutation::RemoveChild { parent, child } => {
                let Some((parent, child)) = script_to_dom
                    .get(&parent)
                    .copied()
                    .zip(script_to_dom.get(&child).copied())
                else {
                    continue;
                };
                if mutator.parent_id(child) == Some(parent) {
                    mutator.remove_node(child);
                }
            }
            Mutation::SetText { node, text } => {
                if let Some(node) = script_to_dom.get(&node).copied() {
                    mutator.set_node_text(node, &text);
                }
            }
            Mutation::SetAttribute { node, name, value } => {
                let Some(node) = script_to_dom.get(&node).copied() else {
                    continue;
                };
                mutator.set_attribute(
                    node,
                    QualName::new(None, ns!(html), LocalName::from(name)),
                    &value,
                );
            }
            Mutation::RemoveAttribute { node, name } => {
                let Some(node) = script_to_dom.get(&node).copied() else {
                    continue;
                };
                mutator
                    .clear_attribute(node, QualName::new(None, ns!(html), LocalName::from(name)));
            }
        }
    }
}

fn apply_tracked_surface_styles(
    document: &blitz_dom::BaseDocument,
    placements: &mut [GamePlacement],
) {
    let styles = document
        .tree()
        .iter()
        .filter_map(|(node_id, node)| {
            let element = node.element_data()?;
            if !is_tracked_surface_element(element) {
                return None;
            }
            let source = element.attr(local_name!("src"))?.parse::<u64>().ok()?;
            let radius = element
                .attr(LocalName::from("data-corner-radius"))
                .and_then(|value| value.parse::<f32>().ok())
                .unwrap_or_default()
                .max(0.0);
            let styles = node.primary_styles()?;
            Some((
                source,
                (
                    node_id,
                    radius,
                    GameViewStyle {
                        object_fit: styles.clone_object_fit(),
                        object_position: styles.clone_object_position(),
                    },
                ),
            ))
        })
        .collect::<HashMap<_, _>>();
    for placement in placements {
        let maximum = (placement.rect.width().min(placement.rect.height()) * 0.5) as f32;
        placement.corner_radius = styles
            .get(&placement.source_id)
            .map(|(_, radius, _)| *radius)
            .unwrap_or_default()
            .min(maximum);
        placement.style = styles
            .get(&placement.source_id)
            .map(|(_, _, style)| style.clone());
        placement.node_id = styles
            .get(&placement.source_id)
            .map(|(node_id, _, _)| *node_id);
        placement.opacity = placement.opacity.clamp(0.0, 1.0);
    }
}

fn initialise_tracked_surfaces(document: &mut blitz_html::HtmlDocument) {
    let surfaces = document
        .as_mut()
        .tree()
        .iter()
        .filter_map(|(node, current)| {
            let element = current.element_data()?;
            if element.name.local.as_ref() != "tracked-surface" {
                return None;
            }
            let source = element.attr(local_name!("src"))?.parse::<u64>().ok()?;
            Some((node, source))
        })
        .collect::<Vec<_>>();
    for (node, source) in surfaces {
        set_tracked_surface_source(document, node, Some(source));
    }
}

fn set_tracked_surface_source(
    document: &mut blitz_html::HtmlDocument,
    node: usize,
    source: Option<u64>,
) {
    document
        .as_mut()
        .set_custom_widget(node, Box::new(TrackedSurfaceWidget::new(source)));
}

fn is_tracked_surface_element(element: &blitz_dom::node::ElementData) -> bool {
    element.name.local.as_ref() == "tracked-surface"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::background_runtime::deno;

    #[test]
    fn tracked_surface_becomes_a_custom_paint_placeholder() {
        let mut document = blitz_html::HtmlDocument::from_html(
            r#"<style>.surface {
                display: block;
                width: 320px;
                height: 180px;
                object-fit: cover;
                object-position: 25% 75%;
                opacity: 0.4;
            }</style><tracked-surface class="surface" src="7" data-corner-radius="12"></tracked-surface>"#,
            blitz_dom::DocumentConfig::default(),
        );
        initialise_tracked_surfaces(&mut document);
        document
            .as_mut()
            .set_viewport(blitz_traits::shell::Viewport::new(
                800,
                600,
                1.0,
                blitz_traits::shell::ColorScheme::Dark,
            ));
        document.as_mut().resolve(1.0);
        let (surface_id, surface) = document
            .as_ref()
            .tree()
            .iter()
            .find_map(|(node_id, node)| {
                node.element_data()
                    .filter(|element| is_tracked_surface_element(element))
                    .map(|element| (node_id, element))
            })
            .unwrap();
        assert!(surface.custom_widget_data().is_some());
        let mut scenes = (0..2).map(|_| Scene::new()).collect::<Vec<_>>();
        let mut painters = scenes
            .iter_mut()
            .map(VelloScenePainter::new)
            .collect::<Vec<_>>();
        let mut placements = Vec::new();
        let mut scene = BandedScene::new(&mut painters, &mut placements);
        blitz_paint::paint_scene(&mut scene, document.as_mut(), 1.0, 800, 600, 0, 0);
        apply_tracked_surface_styles(document.as_ref(), &mut placements);
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].node_id, Some(surface_id));
        assert_eq!(placements[0].source_id, 7);
        assert_eq!(placements[0].rect.width(), 320.0);
        assert_eq!(placements[0].rect.height(), 180.0);
        let clipping = placements[0]
            .clipping
            .expect("the containing paint layer publishes its clip");
        assert!(clipping.contains(placements[0].rect.origin()));
        assert_eq!(placements[0].corner_radius, 12.0);
        assert!((placements[0].opacity - 0.4).abs() < f32::EPSILON);
        let style = placements[0].style.as_ref().unwrap();
        assert_eq!(
            style.object_fit,
            style::properties::generated::longhands::object_fit::computed_value::T::Cover
        );
        assert_eq!(
            style
                .object_position
                .horizontal
                .resolve(style::values::computed::CSSPixelLength::new(100.0))
                .px(),
            25.0
        );
        assert_eq!(
            style
                .object_position
                .vertical
                .resolve(style::values::computed::CSSPixelLength::new(100.0))
                .px(),
            75.0
        );
    }

    #[test]
    fn pointer_hit_skips_disabled_overlays_but_keeps_reenabled_children() {
        let html = r#"
            <style>
              :root, body { width: 100%; height: 100%; margin: 0; }
              .surface, .overlay, .shield { position: absolute; inset: 0; }
              .overlay { z-index: 20; pointer-events: none; }
              .shield { pointer-events: none; }
              .button {
                position: absolute;
                left: 10px;
                top: 10px;
                width: 40px;
                height: 40px;
                pointer-events: auto;
              }
            </style>
            <tracked-surface class="surface" id="surface" src="0"></tracked-surface>
            <div class="overlay">
              <div class="shield"></div>
              <div class="button" id="button"></div>
            </div>
        "#;
        let mut document =
            blitz_html::HtmlDocument::from_html(html, blitz_dom::DocumentConfig::default());
        document
            .as_mut()
            .set_viewport(blitz_traits::shell::Viewport::new(
                800,
                600,
                1.0,
                blitz_traits::shell::ColorScheme::Dark,
            ));
        document.as_mut().resolve(1.0);
        let find = |id| {
            document
                .as_ref()
                .tree()
                .iter()
                .find_map(|(node, current)| {
                    (current.attr(local_name!("id")) == Some(id)).then_some(node)
                })
                .unwrap()
        };
        let surface = find("surface");
        let button = find("button");

        assert_eq!(pointer_hit(document.as_ref(), 20.0, 20.0), Some(button));
        assert_eq!(pointer_hit(document.as_ref(), 100.0, 100.0), Some(surface));
    }

    #[test]
    fn styles_new_react_subtrees_before_attribute_invalidation() {
        let html = r#"
            <style>
              .stage { display: flex; width: 400px; height: 200px; }
              .child { flex: 1; }
            </style>
            <div id="root"></div>
        "#;
        let mut document =
            blitz_html::HtmlDocument::from_html(html, blitz_dom::DocumentConfig::default());
        document
            .as_mut()
            .set_viewport(blitz_traits::shell::Viewport::new(
                800,
                600,
                1.0,
                blitz_traits::shell::ColorScheme::Dark,
            ));
        document.as_mut().resolve(1.0);
        let root = document
            .as_ref()
            .tree()
            .iter()
            .find_map(|(id, node)| (node.attr(local_name!("id")) == Some("root")).then_some(id))
            .unwrap();
        let mut script_to_dom = document
            .as_ref()
            .tree()
            .iter()
            .map(|(id, _)| (id, id))
            .collect::<HashMap<_, _>>();
        let mut dom_to_script = script_to_dom.clone();
        let stage = script_to_dom.keys().copied().max().unwrap() + 1;
        let child = stage + 1;

        apply_dom_mutations(
            &mut document,
            &mut script_to_dom,
            &mut dom_to_script,
            vec![
                deno::Mutation::CreateElement {
                    node: stage,
                    tag: "section".to_owned(),
                },
                deno::Mutation::CreateElement {
                    node: child,
                    tag: "div".to_owned(),
                },
                deno::Mutation::AppendChild {
                    parent: stage,
                    child,
                },
                deno::Mutation::AppendChild {
                    parent: root,
                    child: stage,
                },
                deno::Mutation::SetAttribute {
                    node: stage,
                    name: "class".to_owned(),
                    value: "stage".to_owned(),
                },
                deno::Mutation::SetAttribute {
                    node: child,
                    name: "class".to_owned(),
                    value: "child".to_owned(),
                },
            ],
        );
        document.as_mut().resolve(1.0);

        let stage = script_to_dom[&stage];
        assert_eq!(
            document
                .as_ref()
                .tree()
                .get(stage)
                .unwrap()
                .attr(local_name!("class")),
            Some("stage")
        );
    }

    #[tokio::test]
    async fn dynamically_inserted_tracked_surfaces_are_placed() {
        let html = r##"
            <style>
              :root, body, #root { width: 100%; height: 100%; margin: 0; }
              .stage { position: absolute; inset: 0; }
              .primary { position: absolute; inset: 16px; }
              .secondary {
                position: absolute;
                right: 24px;
                bottom: 24px;
                width: 200px;
                height: 100px;
              }
              .surface { position: absolute; inset: 0; width: 100%; height: 100%; }
            </style>
            <div id="root"></div>
            <script>
              const stage = document.createElement("section");
              stage.className = "stage";
              for (const [index, className] of [[0, "primary"], [1, "secondary"]]) {
                const frame = document.createElement("div");
                frame.className = className;
                const surface = document.createElement("tracked-surface");
                surface.className = "surface";
                surface.setAttribute("src", String(index));
                frame.appendChild(surface);
                stage.appendChild(frame);
              }
              document.querySelector("#root").appendChild(stage);
            </script>
        "##;
        let mut document =
            blitz_html::HtmlDocument::from_html(html, blitz_dom::DocumentConfig::default());
        document
            .as_mut()
            .set_viewport(blitz_traits::shell::Viewport::new(
                800,
                600,
                1.0,
                blitz_traits::shell::ColorScheme::Dark,
            ));
        document.as_mut().resolve(1.0);
        let mut script_to_dom = document
            .as_ref()
            .tree()
            .iter()
            .map(|(id, _)| (id, id))
            .collect::<HashMap<_, _>>();
        let mut dom_to_script = script_to_dom.clone();
        let mut runtime = deno::Runtime::new(
            document.as_ref(),
            html,
            "snowglass:///test.html",
            800,
            600,
            deno::EmulatorCommandSink::default(),
        )
        .await
        .unwrap();
        apply_dom_mutations(
            &mut document,
            &mut script_to_dom,
            &mut dom_to_script,
            runtime.take_mutations(),
        );
        document.as_mut().resolve(1.0);

        let mut scenes = (0..3).map(|_| Scene::new()).collect::<Vec<_>>();
        let mut painters = scenes
            .iter_mut()
            .map(VelloScenePainter::new)
            .collect::<Vec<_>>();
        let mut placements = Vec::new();
        let mut scene = BandedScene::new(&mut painters, &mut placements);
        blitz_paint::paint_scene(&mut scene, document.as_mut(), 1.0, 800, 600, 0, 0);
        assert_eq!(
            placements
                .iter()
                .map(|placement| placement.source_id)
                .collect::<Vec<_>>(),
            [0, 1]
        );
        assert_eq!(placements[0].rect.width(), 768.0);
        assert_eq!(placements[0].rect.height(), 568.0);
        assert_eq!(placements[1].rect.width(), 200.0);
        assert_eq!(placements[1].rect.height(), 100.0);
    }
}
