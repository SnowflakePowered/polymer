//! Lays out and paints the game layer with Blitz.
//!
//! Everything between the background and the foreground WebView2 lives here:
//! the backdrop, the bezel art, and the boxes the guest frames are shown in.
//! The foreground stays in WebView2 because it needs the whole web platform;
//! this layer needs CSS layout and nothing else, so it can be driven by a
//! renderer we control and composited without a browser in the path.
//!
//! A game view is written as `<canvas src="N">`, where N is the tracked render
//! target. Painting diverts those into rectangles ([`bands::GamePlacement`])
//! and splits the remaining chrome into bands, which the compositor stacks
//! around them.

mod bands;
mod drag;
mod shared;

use anyrender_vello::VelloScenePainter;
use bands::BandedScene;
pub use bands::GamePlacement;
use blitz_dom::net::Resource;
use blitz_traits::net::{BoxedHandler, NetProvider, Request, SharedCallback};
use drag::Drag;
pub use shared::SharedChrome;
use shared::SharedTexture;
use std::{
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
/// The built-in document uses the private `snowglass:` scheme for assets
/// embedded in the executable. An override may reference files beside itself.
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
    /// The box the pointer is currently moving, if any.
    dragging: Option<Drag>,
    /// Where the current drag last put it, so an unchanged position is not
    /// re-applied and does not cost a repaint.
    dragged_to: Option<(f32, f32)>,
}

impl GameLayer {
    /// `views` is how many game views the document can show; it fixes the
    /// number of chrome bands, which is one more than that. The document names
    /// each one by its `<canvas src>`, so this is a capacity, not a mapping.
    pub fn new(
        html: &str,
        base_url: String,
        views: usize,
        width: u32,
        height: u32,
    ) -> Result<Self, Box<dyn Error>> {
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
        let document = blitz_html::HtmlDocument::from_html(
            html,
            blitz_dom::DocumentConfig {
                base_url: Some(base_url),
                net_provider: Some(net),
                ..Default::default()
            },
        );
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

        let mut layer = Self {
            document,
            arrivals,
            bands: (0..=views).map(|_| Scene::new()).collect(),
            renderer,
            device,
            queue,
            targets: (0..=views).map(|_| None).collect(),
            placements: Vec::new(),
            width: 0,
            height: 0,
            dirty: true,
            dragging: None,
            dragged_to: None,
        };
        layer.resize(width, height);
        Ok(layer)
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

    /// Whether a point lands on something the pointer may move, and if so,
    /// takes hold of it.
    ///
    /// A point inside a game view is never a handle: the pointer belongs to the
    /// guest there, so only the chrome around it grabs.
    pub fn grab(&mut self, x: f32, y: f32) -> bool {
        let document = self.document.as_ref();
        let Some(node) = drag::find(document, x, y) else {
            return false;
        };
        self.dragging = Some(Drag::start(document, node, x, y, (self.width, self.height)));
        true
    }

    /// Moves whatever was grabbed. Returns whether anything actually shifted,
    /// so a pointer that has not moved far enough to change the layout does not
    /// cost a repaint.
    pub fn drag_to(&mut self, x: f32, y: f32) -> bool {
        let Some(drag) = self.dragging else {
            return false;
        };
        let offset = drag.offset_for(x, y);
        if self.dragged_to == Some(offset) {
            return false;
        }
        self.dragged_to = Some(offset);
        drag::apply(self.document.as_mut(), drag.node(), offset);
        self.dirty = true;
        true
    }

    pub fn release(&mut self) {
        self.dragging = None;
        self.dragged_to = None;
    }

    pub fn is_dragging(&self) -> bool {
        self.dragging.is_some()
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
}
