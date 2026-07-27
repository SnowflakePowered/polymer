//! Lays out and paints the game layer with Blitz.
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
mod deno;
mod shared;

use anyrender_vello::VelloScenePainter;
use bands::BandedScene;
pub use bands::GamePlacement;
use blitz_dom::net::Resource;
use blitz_dom::node::{CanvasData, SpecialElementData};
use blitz_dom::{Document as _, LocalName, QualName, local_name, ns};
use blitz_traits::events::{BlitzMouseButtonEvent, MouseEventButton, MouseEventButtons, UiEvent};
use blitz_traits::net::{BoxedHandler, NetProvider, Request, SharedCallback};
pub use deno::Command as GameLayerCommand;
pub use shared::SharedChrome;
use shared::SharedTexture;
use std::{
    collections::HashMap,
    error::Error,
    sync::{
        Arc,
        mpsc::{Receiver, Sender, channel},
    },
};
use vello::wgpu;
use vello::{AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene};

/// How many times a paint will re-resolve to pick up assets that only became
/// reachable through an asset it just loaded.
const ASSET_ROUNDS: usize = 4;

/// Set to report what each repaint of this layer costs.
const PAINT_TIMING: &str = "SNOWGLASS_GAME_LAYER_TIMING";

/// Loads the layer's own assets, and only those.
///
/// A linked document can use the private `snowglass:` scheme for assets
/// embedded in the executable, while a `file:` document may reference files
/// beside itself.
/// There is nothing to fetch over a network, so `blitz-net` would bring an
/// async runtime along to do less than this.
struct LocalAssets {
    callback: SharedCallback<Resource>,
}

impl NetProvider<Resource> for LocalAssets {
    fn fetch(&self, doc_id: usize, request: Request, handler: BoxedHandler<Resource>) {
        let url = request.url;
        let bytes = match url.scheme() {
            "file" => {
                let Ok(path) = url.to_file_path() else {
                    eprintln!("snowglass: the game layer cannot resolve {url} to a path");
                    return;
                };
                match std::fs::read(&path) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        eprintln!(
                            "snowglass: the game layer could not read {}: {error}",
                            path.display()
                        );
                        return;
                    }
                }
            }
            "snowglass" if url.path() == "/assets/wiiu-drc.png" => {
                include_bytes!("../frontend/src/assets/wiiu-drc.png").to_vec()
            }
            _ => {
                eprintln!("snowglass: the game layer will not fetch {url}");
                return;
            }
        };
        // Parsing the bytes into an image is the handler's job; it hands the
        // result to the callback, which queues it for the next paint.
        handler.bytes(doc_id, bytes.into(), Arc::clone(&self.callback));
    }
}

pub struct GameLayer {
    document: blitz_html::HtmlDocument,
    /// Assets arrive here because a fetch is issued while the document is
    /// mid-resolve and cannot take them straight away.
    arrivals: Receiver<Resource>,
    /// One encoded scene per band. Encoding is cheap and happens on the CPU;
    /// only rasterising is worth a GPU.
    bands: Vec<Scene>,
    renderer: Renderer,
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Where each band is rasterised. Created on demand, because a document
    /// showing two views never touches the capacity kept for eight.
    targets: Vec<Option<SharedTexture>>,
    placements: Vec<GamePlacement>,
    width: u32,
    height: u32,
    /// Set whenever layout or content changed, so a paint is only spent when
    /// something can differ. Guest frames arriving do not dirty this layer.
    dirty: bool,
    /// The Deno isolate which runs scripts belonging to this document.
    runtime: Option<deno::Runtime>,
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

/// GPU state prepared before WebView2 starts its renderer.
///
/// Creating Vello's D3D12 device and pipelines after Chromium has initialized
/// its WARP path can crash inside `d3d10warp.dll`. The foreground URL still
/// decides whether a layer document exists; this only reserves the renderer
/// that will paint it if one is discovered.
pub struct GameLayerFactory {
    renderer: Renderer,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

impl GameLayerFactory {
    pub fn new() -> Result<Self, Box<dyn Error>> {
        // A device of its own rather than the compositor's: this rasterises
        // rarely and off the present path, so it has nothing to gain from
        // sharing a queue with the frames going to screen.
        //
        // D3D12 specifically, and not whichever backend wgpu would have picked.
        // The compositor is D3D12, and a texture only passes between devices of
        // the same API, which is the whole point of rendering into one here.
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::DX12,
            ..Default::default()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .map_err(|error| format!("the game layer found no D3D12 adapter: {error}"))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("snowglass game layer"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::default(),
        }))
        .map_err(|error| format!("the game layer could not open a D3D12 device: {error}"))?;
        let renderer = Renderer::new(
            &device,
            RendererOptions {
                use_cpu: false,
                num_init_threads: None,
                // The layer is boxes, text and images; the full antialiasing
                // set would cost pipelines it never uses.
                antialiasing_support: AaSupport::area_only(),
                pipeline_cache: None,
            },
        )
        .map_err(|error| format!("the game layer could not start Vello: {error}"))?;

        Ok(Self {
            renderer,
            device,
            queue,
        })
    }

    /// `views` is how many game views the document can show; it fixes the
    /// number of chrome bands, which is one more than that. The document names
    /// each one by its `<tracked-surface src>`, so this is a capacity, not a
    /// mapping.
    pub fn create(
        self,
        html: &str,
        base_url: String,
        views: usize,
        width: u32,
        height: u32,
    ) -> GameLayer {
        let (sender, arrivals) = channel();
        let net = Arc::new(LocalAssets {
            callback: Arc::new(
                move |_doc: usize, result: Result<Resource, Option<String>>| {
                    if let Ok(resource) = result {
                        let _ = Sender::send(&sender, resource);
                    }
                },
            ),
        });
        let mut document = blitz_html::HtmlDocument::from_html(
            html,
            blitz_dom::DocumentConfig {
                base_url: Some(base_url.clone()),
                net_provider: Some(net),
                ..Default::default()
            },
        );
        initialise_tracked_surfaces(&mut document);
        self.build(document, arrivals, html, &base_url, views, (width, height))
    }
}

impl GameLayerFactory {
    fn build(
        self,
        document: blitz_html::HtmlDocument,
        arrivals: Receiver<Resource>,
        html: &str,
        source_name: &str,
        views: usize,
        viewport: (u32, u32),
    ) -> GameLayer {
        let (width, height) = viewport;
        let script_to_dom = document
            .as_ref()
            .tree()
            .iter()
            .map(|(id, _)| (id, id))
            .collect::<HashMap<_, _>>();
        let dom_to_script = script_to_dom.clone();
        let mut layer = GameLayer {
            document,
            arrivals,
            bands: (0..=views).map(|_| Scene::new()).collect(),
            renderer: self.renderer,
            device: self.device,
            queue: self.queue,
            targets: (0..=views).map(|_| None).collect(),
            placements: Vec::new(),
            width: 0,
            height: 0,
            dirty: true,
            runtime: None,
            script_to_dom,
            dom_to_script,
            mouse_down: None,
        };
        layer.resize(width, height);
        // Scripts run after parsing, with a complete DOM just like deferred
        // document scripts. Resolve once first so geometry APIs have useful
        // values during initialisation.
        layer.document.as_mut().resolve(1.0);
        match deno::Runtime::new(
            layer.document.as_ref(),
            html,
            source_name,
            layer.width,
            layer.height,
        ) {
            Ok(runtime) => layer.runtime = Some(runtime),
            Err(error) => {
                eprintln!("snowglass: starting game-layer JavaScript failed: {error}");
            }
        }
        layer.apply_script_mutations();
        layer
    }
}

impl GameLayer {
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
        // Every band's target is the wrong size now. They are dropped rather
        // than resized, and the next paint makes only the ones it uses.
        for target in &mut self.targets {
            *target = None;
        }
        self.dirty = true;
    }

    pub fn placements(&self) -> &[GamePlacement] {
        &self.placements
    }

    /// Forces the next paint even when the document itself did not change.
    ///
    /// This is needed when the shell changes where the existing pixels are
    /// composited, such as when the first stream replaces DirectComposition
    /// chrome bands with chrome inside the shared D3D12 swapchain.
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn needs_paint(&self) -> bool {
        self.dirty
    }

    /// Dispatches a mouse event through the JavaScript DOM. The return value is
    /// true when script called `preventDefault`, which claims the pointer from
    /// the guest surface below this layer.
    pub fn mouse_event(&mut self, event: DomMouseEvent<'_>) -> bool {
        let DomMouseEvent {
            kind,
            x,
            y,
            button,
            buttons,
            ctrl_key,
            shift_key,
        } = event;
        let Some(target) = self.document.as_ref().hit(x, y).map(|hit| hit.node_id) else {
            return false;
        };
        let targets_game_view = std::iter::successors(Some(target), |node| {
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
        let Some(runtime) = self.runtime.as_mut() else {
            return false;
        };
        let native_event = deno::MouseEvent {
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
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                eprintln!("snowglass: dispatching game-layer {kind} failed: {error}");
                Default::default()
            }
        };
        let mut claimed = result.default_prevented || (result.handled && !targets_game_view);
        if kind == "mousedown" {
            self.mouse_down = Some(target);
        } else if kind == "mouseup" {
            let clicked = button == 0 && self.mouse_down.take() == Some(target);
            if clicked {
                match runtime.dispatch_mouse(
                    "click",
                    script_target,
                    deno::MouseEvent {
                        client_x: x,
                        client_y: y,
                        button,
                        buttons,
                        ctrl_key,
                        shift_key,
                        alt_key: false,
                        meta_key: false,
                    },
                ) {
                    Ok(click) => {
                        claimed |= click.default_prevented || (click.handled && !targets_game_view);
                    }
                    Err(error) => {
                        eprintln!("snowglass: dispatching game-layer click failed: {error}");
                    }
                }
            }
        }
        self.apply_script_mutations();
        if !result.default_prevented {
            let data = BlitzMouseButtonEvent {
                x,
                y,
                button: match button {
                    1 => MouseEventButton::Auxiliary,
                    2 => MouseEventButton::Secondary,
                    3 => MouseEventButton::Fourth,
                    4 => MouseEventButton::Fifth,
                    _ => MouseEventButton::Main,
                },
                buttons: MouseEventButtons::from_bits_truncate(buttons),
                mods: Default::default(),
            };
            let event = match kind {
                "mousedown" => Some(UiEvent::MouseDown(data)),
                "mouseup" => Some(UiEvent::MouseUp(data)),
                "mousemove" => Some(UiEvent::MouseMove(data)),
                _ => None,
            };
            if let Some(event) = event {
                self.document.handle_ui_event(event);
            }
        }
        claimed
    }

    /// Delivers a structured message from the foreground WebView.
    pub fn post_message(&mut self, message: &serde_json::Value) {
        let Some(runtime) = self.runtime.as_mut() else {
            return;
        };
        if let Err(error) = runtime.post_message(message) {
            eprintln!("snowglass: delivering a game-layer message failed: {error}");
        }
        self.apply_script_mutations();
    }

    /// Messages posted by the game-layer document for the foreground.
    pub fn take_messages(&mut self) -> Vec<serde_json::Value> {
        self.runtime
            .as_mut()
            .map(deno::Runtime::take_messages)
            .unwrap_or_default()
    }

    pub fn proxy_request(&mut self, request: &serde_json::Value) -> Result<(), String> {
        let runtime = self
            .runtime
            .as_mut()
            .ok_or_else(|| "the background JavaScript runtime is unavailable".to_owned())?;
        runtime
            .proxy_request(request)
            .map_err(|error| error.to_string())?;
        self.apply_script_mutations();
        Ok(())
    }

    pub fn take_proxy_messages(&mut self) -> Vec<serde_json::Value> {
        self.runtime
            .as_mut()
            .map(deno::Runtime::take_proxy_messages)
            .unwrap_or_default()
    }

    /// Verifies that the attachment is the background tracked-surface element
    /// whose `src` names this tracked render-target index.
    pub fn validates_attachment(&self, attachment: Option<usize>, index: u32) -> bool {
        attachment
            .and_then(|node| self.script_to_dom.get(&node).copied())
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

    pub fn take_commands(&mut self) -> Vec<deno::Command> {
        self.runtime
            .as_mut()
            .map(deno::Runtime::take_commands)
            .unwrap_or_default()
    }

    pub fn dispatch_game_view_event(&mut self, event: &serde_json::Value) {
        let Some(runtime) = self.runtime.as_mut() else {
            return;
        };
        if let Err(error) = runtime.dispatch_game_view_event(event) {
            eprintln!("snowglass: dispatching a background GameView event failed: {error}");
        }
        self.apply_script_mutations();
    }

    /// Advances timers, fetches, WebSockets, promises, and inspector traffic
    /// without waiting for the JavaScript realm to become idle.
    pub fn poll_runtime(&mut self) -> bool {
        let Some(runtime) = self.runtime.as_mut() else {
            return false;
        };
        if let Err(error) = runtime.poll_event_loop() {
            eprintln!("snowglass: polling game-layer JavaScript failed: {error}");
        }
        self.apply_script_mutations();
        true
    }

    /// Re-lays out and repaints when something changed, handing each band to
    /// `band` as a texture the compositor can open. Returns whether a paint
    /// happened.
    pub fn paint(&mut self, mut band: impl FnMut(usize, SharedChrome)) -> bool {
        if !self.take_arrivals() && !self.dirty {
            return false;
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
        if let Some(runtime) = self.runtime.as_mut()
            && let Err(error) = runtime.sync(
                self.document.as_ref(),
                self.width,
                self.height,
                &self.dom_to_script,
            )
        {
            eprintln!("snowglass: synchronising the game-layer DOM failed: {error}");
        }

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
            blitz_paint::paint_scene(
                &mut scene,
                self.document.as_ref(),
                1.0,
                self.width,
                self.height,
            );
            (scene.bands_used(), scene.overflowed())
        };
        if overflowed {
            eprintln!(
                "snowglass: the game layer shows more than {} views, so the ones past that \
                 cannot be stacked against the chrome between them",
                self.bands.len() - 1
            );
        }
        let encoded = started.elapsed();
        let rasterised = std::time::Instant::now();
        // Transparent rather than a colour: a band is only the chrome falling
        // in its slice, and everything below it has to show through.
        let parameters = RenderParams {
            base_color: vello::peniko::Color::TRANSPARENT,
            width: self.width,
            height: self.height,
            antialiasing_method: AaConfig::Area,
        };
        let mut drawn = Vec::with_capacity(used);
        for index in 0..used {
            let target = match &mut self.targets[index] {
                Some(target) => target,
                slot => match SharedTexture::new(&self.device, self.width, self.height) {
                    Ok(target) => slot.insert(target),
                    Err(error) => {
                        eprintln!("snowglass: the game layer could not make band {index}: {error}");
                        continue;
                    }
                },
            };
            if let Err(error) = self.renderer.render_to_texture(
                &self.device,
                &self.queue,
                &self.bands[index],
                &target.view,
                &parameters,
            ) {
                eprintln!("snowglass: rasterising game layer band {index} failed: {error}");
                continue;
            }
            drawn.push((index, target.shared));
        }
        // The compositor reads these on its own device and its own queue, with
        // no fence between them. Waiting here is what makes that safe, and it
        // costs nothing in practice: this layer repaints on layout changes, not
        // on frames.
        if let Err(error) = self.device.poll(wgpu::PollType::Wait) {
            eprintln!("snowglass: waiting for the game layer to finish failed: {error}");
        }
        for (index, shared) in drawn {
            band(index, shared);
        }
        if std::env::var_os(PAINT_TIMING).is_some() {
            eprintln!(
                "snowglass: game layer painted {}x{} in {:.2}ms encode + {:.2}ms rasterise                  over {used} band(s)",
                self.width,
                self.height,
                encoded.as_secs_f64() * 1000.0,
                rasterised.elapsed().as_secs_f64() * 1000.0,
            );
        }
        true
    }
}

impl GameLayer {
    /// Hands the document every asset that has finished loading, reporting
    /// whether any did.
    fn take_arrivals(&mut self) -> bool {
        let mut arrived = false;
        while let Ok(resource) = self.arrivals.try_recv() {
            self.document.as_mut().load_resource(resource);
            arrived = true;
        }
        arrived
    }

    fn apply_script_mutations(&mut self) {
        let mutations = self
            .runtime
            .as_mut()
            .map(deno::Runtime::take_mutations)
            .unwrap_or_default();
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
    mutations: Vec<deno::Mutation>,
) -> bool {
    if mutations.is_empty() {
        return false;
    }
    let mut mutator = document.as_mut().mutate();
    for mutation in mutations {
        match mutation {
            deno::Mutation::CreateElement { node, tag } => {
                let actual = mutator.create_element(
                    QualName::new(None, ns!(html), LocalName::from(tag)),
                    Vec::new(),
                );
                script_to_dom.insert(node, actual);
                dom_to_script.insert(actual, node);
            }
            deno::Mutation::CreateText { node, text } => {
                let actual = mutator.create_text_node(&text);
                script_to_dom.insert(node, actual);
                dom_to_script.insert(actual, node);
            }
            deno::Mutation::CreateComment { node } => {
                let actual = mutator.create_comment_node();
                script_to_dom.insert(node, actual);
                dom_to_script.insert(actual, node);
            }
            deno::Mutation::AppendChild { parent, child } => {
                let Some((parent, child)) = script_to_dom
                    .get(&parent)
                    .copied()
                    .zip(script_to_dom.get(&child).copied())
                else {
                    continue;
                };
                mutator.append_children(parent, &[child]);
            }
            deno::Mutation::InsertBefore {
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
            deno::Mutation::RemoveChild { parent, child } => {
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
            deno::Mutation::SetText { node, text } => {
                if let Some(node) = script_to_dom.get(&node).copied() {
                    mutator.set_node_text(node, &text);
                }
            }
            deno::Mutation::SetAttribute { node, name, value } => {
                let Some(node) = script_to_dom.get(&node).copied() else {
                    continue;
                };
                let is_source = name == "src";
                mutator.set_attribute(
                    node,
                    QualName::new(None, ns!(html), LocalName::from(name)),
                    &value,
                );
                if is_source {
                    set_tracked_surface_element(
                        mutator.doc.get_node_mut(node),
                        value.parse::<u64>().ok(),
                    );
                }
            }
            deno::Mutation::RemoveAttribute { node, name } => {
                let Some(node) = script_to_dom.get(&node).copied() else {
                    continue;
                };
                let is_source = name == "src";
                mutator
                    .clear_attribute(node, QualName::new(None, ns!(html), LocalName::from(name)));
                if is_source {
                    set_tracked_surface_element(mutator.doc.get_node_mut(node), None);
                }
            }
        }
    }
    true
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
    set_tracked_surface_element(document.as_mut().get_node_mut(node), source);
}

fn set_tracked_surface_element(node: Option<&mut blitz_dom::Node>, source: Option<u64>) {
    let Some(element) = node
        .and_then(blitz_dom::Node::element_data_mut)
        .filter(|element| is_tracked_surface_element(element))
    else {
        return;
    };
    element.special_data = match source {
        Some(source) => SpecialElementData::Canvas(CanvasData {
            custom_paint_source_id: source,
        }),
        None => SpecialElementData::None,
    };
}

fn is_tracked_surface_element(element: &blitz_dom::node::ElementData) -> bool {
    element.name.local.as_ref() == "tracked-surface"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracked_surface_becomes_a_custom_paint_placeholder() {
        let mut document = blitz_html::HtmlDocument::from_html(
            r#"<style>.surface { display: block; width: 320px; height: 180px; }</style><tracked-surface class="surface" src="7"></tracked-surface>"#,
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
        let surface = document
            .as_ref()
            .tree()
            .iter()
            .find_map(|(_, node)| {
                node.element_data()
                    .filter(|element| is_tracked_surface_element(element))
            })
            .unwrap();
        assert_eq!(
            surface
                .canvas_data()
                .map(|canvas| canvas.custom_paint_source_id),
            Some(7)
        );
        let mut scenes = (0..2).map(|_| Scene::new()).collect::<Vec<_>>();
        let mut painters = scenes
            .iter_mut()
            .map(VelloScenePainter::new)
            .collect::<Vec<_>>();
        let mut placements = Vec::new();
        let mut scene = BandedScene::new(&mut painters, &mut placements);
        blitz_paint::paint_scene(&mut scene, document.as_ref(), 1.0, 800, 600);
        assert_eq!(placements.len(), 1);
        assert_eq!(placements[0].source_id, 7);
        assert_eq!(placements[0].rect.width(), 320.0);
        assert_eq!(placements[0].rect.height(), 180.0);
    }

    #[test]
    fn dynamically_inserted_tracked_surfaces_are_placed() {
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
        let mut runtime =
            deno::Runtime::new(document.as_ref(), html, "snowglass:///test.html", 800, 600)
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
        blitz_paint::paint_scene(&mut scene, document.as_ref(), 1.0, 800, 600);
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
