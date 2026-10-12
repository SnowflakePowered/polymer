use crate::window::SurfaceWindow;
use polymer_composition::SurfaceFrameEvent;
use polymer_protocol::AdapterLuid;
use polymer_renderer::layout::Rect;
use polymer_renderer::{
    CompositingPlan, Extent, ReceivedSurface, Renderer, SurfaceContent, SurfacePlacement,
};
use polymer_renderer_d3d12::D3d12Compositor;
use polymer_runtime::{SurfaceFrameReceiver, SurfaceOffer};
use std::{collections::HashMap, error::Error, sync::mpsc, time::Duration};
use windows::{
    Win32::Graphics::Direct3D12::ID3D12Device,
    core::{IUnknown, Interface},
};
mod group;
mod video;
use group::{GroupEvents, SurfaceGroup};
use polymer_composition::{ShaderContent, ShaderControl, SurfaceGroupLayout, SurfaceGroupMember};
use video::VideoSurface;
pub(crate) struct RenderDevice {
    adapter: AdapterLuid,
    device: IUnknown,
    surface_compositors: HashMap<u64, SurfaceCompositor>,
    videos: HashMap<u64, VideoSurface>,
    groups: HashMap<u64, SurfaceGroup>,
    events: mpsc::Receiver<SurfaceFrameEvent>,
    shader_contents: mpsc::Receiver<ShaderContent>,
    group_events: GroupEvents,
    publication: Option<polymer_renderer_d3d12::webrtc::Publication>,
}

struct SurfaceCompositor {
    compositor: Option<D3d12Compositor>,
    content: IUnknown,
    worker: Option<SurfaceGroup>,
    latest_frame: Option<polymer_protocol::FrameReady>,
    group: Option<u64>,
    surface_id: u64,
    target_window: i64,
    quarter_turns: u8,
    flip_y: bool,
    width: u32,
    height: u32,
}

impl SurfaceCompositor {
    /// The stream alone at its own size, as presented without a shader.
    fn native_layout(&self, connection: u64) -> SurfaceGroupLayout {
        let rectangle = Rect::new(0, 0, self.width, self.height);
        SurfaceGroupLayout {
            extent: Extent::new(self.width, self.height),
            members: vec![SurfaceGroupMember {
                connection,
                rectangle,
                corner_radius: 0.0,
                placement: SurfaceContent::fill(rectangle),
            }],
        }
    }
}

impl RenderDevice {
    pub(crate) fn new((adapter, device): (AdapterLuid, IUnknown)) -> Self {
        let (frames, events) = mpsc::channel();
        let (shaders, shader_contents) = mpsc::channel();
        Self {
            adapter,
            device,
            events,
            shader_contents,
            group_events: GroupEvents { frames, shaders },
            publication: None,
            surface_compositors: HashMap::new(),
            videos: HashMap::new(),
            groups: HashMap::new(),
        }
    }

    pub(crate) fn accepts_detached_frames(&self) -> bool {
        self.publication.is_none()
    }

    fn d3d12_device(&self) -> Result<ID3D12Device, windows::core::Error> {
        self.device.cast()
    }

    fn create_surface_compositor(
        &self,
        window: &impl polymer_renderer::RenderWindow,
        connection: u64,
        width: u32,
        height: u32,
    ) -> Result<D3d12Compositor, Box<dyn Error>> {
        let local = SurfaceWindow::new(window, width.max(1), height.max(1));
        D3d12Compositor::new_for_stream_composition(&local, self.adapter, connection)
    }
}

impl RenderDevice {
    pub(crate) fn matches(&self, offer: &SurfaceOffer) -> bool {
        self.adapter == offer.stream().adapter_luid
    }
    pub(crate) fn content(&self, connection: u64) -> Option<IUnknown> {
        self.surface_compositors
            .get(&connection)
            .map(|output| output.content.clone())
            .or_else(|| self.videos.get(&connection).map(VideoSurface::content))
            .or_else(|| self.groups.get(&connection).map(SurfaceGroup::content))
    }
    pub(crate) fn open_video(
        &mut self,
        window: &impl polymer_renderer::RenderWindow,
        id: u64,
        source: polymer_composition::VideoSource,
        sink: polymer_composition::VideoSink,
    ) -> Result<(), Box<dyn Error>> {
        self.videos.remove(&id);
        let compositor = self.create_surface_compositor(window, id, 1, 1)?;
        self.videos
            .insert(id, VideoSurface::spawn(id, compositor, source, sink)?);
        Ok(())
    }
    pub(crate) fn control_video(&self, id: u64, control: polymer_composition::VideoControl) {
        if let Some(video) = self.videos.get(&id) {
            video.control(control);
        }
    }
    pub(crate) fn close_video(&mut self, id: u64) {
        self.videos.remove(&id);
    }
    pub(crate) fn import(
        &mut self,
        window: &impl polymer_renderer::RenderWindow,
        offer: &SurfaceOffer,
        turns: u8,
        group: Option<u64>,
    ) -> Result<Option<u64>, Box<dyn Error>> {
        let stream = offer.stream();
        let (width, height) = if turns % 2 == 1 {
            (stream.height, stream.width)
        } else {
            (stream.width, stream.height)
        };
        let group = group.filter(|_| self.accepts_detached_frames());
        let (compositor, content) = if let Some(group) = group {
            (None, self.import_into_group(window, offer, turns, group)?)
        } else {
            let mut compositor =
                self.create_surface_compositor(window, offer.connection_id(), width, height)?;
            Renderer::import_surface_offer(&mut compositor, offer, turns)?;
            let content = compositor.composition_content()?;
            (Some(compositor), content)
        };
        self.surface_compositors.insert(
            offer.connection_id(),
            SurfaceCompositor {
                compositor,
                content,
                worker: None,
                latest_frame: None,
                group,
                surface_id: stream.surface_id.0,
                target_window: stream.source_window,
                quarter_turns: turns,
                flip_y: stream.bottom_up,
                width: width.max(1),
                height: height.max(1),
            },
        );
        Ok(group)
    }
    fn import_into_group(
        &mut self,
        window: &impl polymer_renderer::RenderWindow,
        offer: &SurfaceOffer,
        turns: u8,
        group: u64,
    ) -> Result<IUnknown, Box<dyn Error>> {
        if !self.groups.contains_key(&group) {
            let local = SurfaceWindow::new(window, 1, 1);
            let compositor = D3d12Compositor::new_for_group_composition(&local, self.adapter)?;
            let spawned = SurfaceGroup::spawn(group, compositor, self.group_events.clone(), &[])?;
            self.groups.insert(group, spawned);
        }
        let connection = offer.connection_id();
        let stream = *offer.stream();
        let shared = offer
            .win32_resources()
            .ok_or("stream resources were revoked")?;
        let surfaces = self.groups.get_mut(&group).expect("group was created");
        let imported = surfaces.import(
            connection,
            stream.surface_id.0,
            Box::new(move |compositor| {
                compositor.import_stream(connection, &stream, &shared, turns)
            }),
        );
        if let Err(error) = imported {
            if surfaces.is_empty() {
                self.groups.remove(&group);
            }
            return Err(error);
        }
        Ok(surfaces.content())
    }
    fn presenter(&self, target: u64) -> Option<&SurfaceGroup> {
        self.groups.get(&target).or_else(|| {
            self.surface_compositors
                .get(&target)
                .and_then(|output| output.worker.as_ref())
        })
    }
    pub(crate) fn arrange_group(&self, group: u64, layout: SurfaceGroupLayout) {
        if let Some(presenter) = self.presenter(group) {
            presenter.arrange(layout);
        }
    }
    pub(crate) fn control_shader(&self, target: u64, control: ShaderControl) -> bool {
        let Some(presenter) = self.presenter(target) else {
            return false;
        };
        if let (ShaderControl::Detach, Some(output)) =
            (&control, self.surface_compositors.get(&target))
        {
            presenter.arrange(output.native_layout(target));
        }
        presenter.control_shader(control);
        true
    }
    pub(crate) fn start_worker(
        &mut self,
        connection: u64,
        frames: SurfaceFrameReceiver,
    ) -> Result<(), Box<dyn Error>> {
        let output = self
            .surface_compositors
            .get_mut(&connection)
            .ok_or("surface is not imported")?;
        if let Some(group) = output.group {
            return self
                .groups
                .get(&group)
                .ok_or("surface group is not running")?
                .attach(connection, frames);
        }
        let compositor = output
            .compositor
            .take()
            .ok_or("surface already has a worker")?;
        let worker = SurfaceGroup::spawn(
            connection,
            compositor,
            self.group_events.clone(),
            &[(connection, output.surface_id)],
        )?;
        worker.arrange(output.native_layout(connection));
        worker.attach(connection, frames)?;
        output.worker = Some(worker);
        Ok(())
    }
    pub(crate) fn register(&mut self, connection: u64, target: u32) -> Result<(), Box<dyn Error>> {
        if let Some(publication) = &mut self.publication {
            publication.add_stream(connection, target)?;
        }
        Ok(())
    }
    pub(crate) fn remove(&mut self, connection: u64) {
        let group = self
            .surface_compositors
            .remove(&connection)
            .and_then(|output| output.group);
        if let Some(group) = group
            && let Some(surfaces) = self.groups.get_mut(&group)
        {
            surfaces.remove(connection);
            if surfaces.is_empty() {
                self.groups.remove(&group);
            }
        }
        if let Some(publication) = &mut self.publication {
            publication.remove_stream(connection);
        }
    }
    pub(crate) fn set_latest(&mut self, connection: u64, frame: polymer_protocol::FrameReady) {
        if let Some(output) = self.surface_compositors.get_mut(&connection) {
            output.latest_frame = Some(frame);
            if let Some(compositor) = &mut output.compositor {
                Renderer::set_latest(compositor, connection, frame);
            }
        }
    }
    pub(crate) fn received(&self, connection: u64) -> Vec<ReceivedSurface> {
        let Some(output) = self.surface_compositors.get(&connection) else {
            return Vec::new();
        };
        let Some(frame) = output.latest_frame else {
            return Vec::new();
        };
        vec![ReceivedSurface {
            connection,
            surface_id: output.surface_id,
            target_window: output.target_window,
            image_extent: Extent::new(output.width, output.height),
            quarter_turns: output.quarter_turns,
            flip_y: output.flip_y,
            frame,
        }]
    }
    pub(crate) fn try_event(&mut self) -> Option<SurfaceFrameEvent> {
        self.events.try_recv().ok()
    }
    pub(crate) fn try_shader_content(&mut self) -> Option<ShaderContent> {
        self.shader_contents.try_recv().ok()
    }
    pub(crate) fn pending(&self) -> bool {
        self.surface_compositors
            .values()
            .filter_map(|output| output.compositor.as_ref())
            .any(Renderer::has_pending_frame)
    }
    pub(crate) fn snapshot(
        &mut self,
        connection: u64,
    ) -> Result<Option<polymer_runtime::RgbaScreenshot>, String> {
        let Some(output) = self.surface_compositors.get_mut(&connection) else {
            return Ok(None);
        };
        if let Some(surfaces) = output.group.and_then(|group| self.groups.get(&group)) {
            return surfaces.snapshot(connection);
        }
        if let Some(worker) = &output.worker {
            return worker.snapshot(connection);
        }
        let Some(compositor) = &mut output.compositor else {
            return Ok(None);
        };
        compositor
            .snapshot_rgba()
            .map(|snapshot| {
                snapshot.map(|(width, height, pixels)| polymer_runtime::RgbaScreenshot {
                    width,
                    height,
                    pixels,
                })
            })
            .map_err(|error| error.to_string())
    }
    pub(crate) fn start_webrtc(
        &mut self,
        bind: std::net::SocketAddr,
        targets: &[u32],
        streams: &[(u64, u32)],
    ) -> Result<(), Box<dyn Error>> {
        let mut publication = polymer_renderer_d3d12::webrtc::Publication::start_d3d12_device(
            bind,
            targets,
            self.adapter,
            &self.d3d12_device()?,
        )?;
        for &(connection, target) in streams {
            publication.add_stream(connection, target)?;
        }
        self.publication = Some(publication);
        Ok(())
    }
    pub(crate) fn stop_webrtc(&mut self) {
        self.publication = None;
    }
    pub(crate) fn replace(
        &mut self,
        context: (AdapterLuid, IUnknown),
    ) -> Result<(), Box<dyn Error>> {
        let mut replacement = Self::new(context);
        if let Some(publication) = &mut self.publication {
            publication.reattach_d3d12_device(replacement.adapter, &replacement.d3d12_device()?)?;
            for &connection in self.surface_compositors.keys() {
                publication.remove_stream(connection);
            }
        }
        replacement.publication = self.publication.take();
        *self = replacement;
        Ok(())
    }
    pub fn draw(
        &mut self,
        corners: &[(u64, f32)],
        permit_wait: Duration,
    ) -> Result<Option<bool>, String> {
        let mut publication = self.publication.as_mut();
        let mut presented = false;
        let mut deferred = false;
        for (&connection, output) in &mut self.surface_compositors {
            let Some(compositor) = output.compositor.as_mut() else {
                continue;
            };
            if !Renderer::has_pending_frame(compositor) {
                continue;
            }
            if !compositor.begin_frame_within(permit_wait) {
                deferred = true;
                continue;
            }
            let Some(received) = Renderer::received_surfaces(compositor).into_iter().next() else {
                continue;
            };
            let rectangle = Rect::new(0, 0, output.width, output.height);
            let mut placement = SurfacePlacement::new(
                connection,
                received.surface_id,
                rectangle,
                corners
                    .iter()
                    .find(|(id, _)| *id == connection)
                    .map_or(0.0, |(_, radius)| *radius),
            );
            placement.content = SurfaceContent::fill(rectangle);
            let plan = CompositingPlan::new([0.0, 0.0, 0.0, 1.0], vec![placement]);
            if let Some(publication) = publication.as_deref_mut() {
                publication
                    .present_d3d12(compositor, &plan)
                    .map_err(|error| error.to_string())?;
            } else {
                compositor
                    .present_with_webrtc(&plan, None)
                    .map_err(|error| error.to_string())?;
            }
            presented = true;
        }
        if presented {
            Ok(Some(true))
        } else if deferred {
            Ok(None)
        } else {
            // Worker-owned tracked surfaces present independently. No main
            // compositor submission happened, so do not report a repeated
            // present or keep the compositor clock active.
            Ok(None)
        }
    }
}
