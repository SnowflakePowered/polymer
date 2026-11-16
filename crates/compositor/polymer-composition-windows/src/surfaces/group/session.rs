use super::{GroupEvents, GroupMessage, IDLE_WAIT};
use polymer_composition::{
    ShaderContent, ShaderControl, SurfaceFrameEvent, SurfaceGroupLayout, SurfaceShader,
};
use polymer_platform_windows::performance_counter;
use polymer_renderer::{CompositingPlan, Renderer, SurfacePlacement};
use polymer_renderer_d3d12::{D3d12Compositor, ShaderStage};
use std::{
    collections::HashMap,
    error::Error,
    sync::mpsc::{self, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

const PERMIT_WAIT: Duration = Duration::from_millis(50);
const MEASURE_DELAY: Duration = Duration::from_millis(250);

type BuiltShader = (u64, Result<ShaderStage, String>);

pub(super) struct GroupSession {
    id: u64,
    compositor: D3d12Compositor,
    events: GroupEvents,
    measure_due: Option<Instant>,
    surfaces: HashMap<u64, u64>,
    layout: SurfaceGroupLayout,
    plan: CompositingPlan,
    stale: bool,
    shader: ShaderState,
}

/// The preset requested last, and the values set on it so far.
#[derive(Default)]
struct ShaderState {
    generation: u64,
    parameters: Vec<(String, f32)>,
    passes: Option<usize>,
    built: Option<(mpsc::Sender<BuiltShader>, mpsc::Receiver<BuiltShader>)>,
}

impl GroupSession {
    pub(super) fn new(
        id: u64,
        compositor: D3d12Compositor,
        events: GroupEvents,
        surfaces: HashMap<u64, u64>,
    ) -> Self {
        Self {
            id,
            compositor,
            events,
            measure_due: None,
            surfaces,
            layout: SurfaceGroupLayout::default(),
            plan: CompositingPlan::new([0.0; 4], Vec::new()),
            stale: false,
            shader: ShaderState::default(),
        }
    }

    pub(super) fn run(
        &mut self,
        messages: &mpsc::Receiver<GroupMessage>,
    ) -> Result<(), Box<dyn Error>> {
        loop {
            let wait = if self.redraw_due() {
                Duration::ZERO
            } else {
                self.measure_due.map_or(IDLE_WAIT, |due| {
                    due.saturating_duration_since(Instant::now()).min(IDLE_WAIT)
                })
            };
            let mut next = match messages.recv_timeout(wait) {
                Ok(message) => Some(message),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            };
            while let Some(message) = next {
                if matches!(message, GroupMessage::Stop) {
                    return Ok(());
                }
                if let Err(error) = self.apply(message) {
                    tracing::error!("polymer: updating a surface group failed: {error}");
                }
                next = messages.try_recv().ok();
            }
            if let Err(error) = self.install_built_shader() {
                tracing::error!("polymer: installing a shader preset failed: {error}");
            }
            self.present()?;
            self.measure();
        }
    }

    /// Reports where the shader drew the composed surfaces, once its layout
    /// and parameters have settled.
    fn measure(&mut self) {
        let Some(due) = self.measure_due else {
            return;
        };
        if Instant::now() < due || self.compositor.shader().is_none() {
            return;
        }
        self.measure_due = None;
        match self.compositor.measure_shader_content() {
            Ok(content) => {
                let _ = self.events.shaders.send(ShaderContent {
                    presenter: self.id,
                    content,
                });
                self.stale = true;
            }
            Err(error) => tracing::error!("polymer: measuring a shader preset failed: {error}"),
        }
    }

    fn remeasure(&mut self) {
        self.measure_due = Some(Instant::now() + MEASURE_DELAY);
    }

    fn apply(&mut self, message: GroupMessage) -> Result<(), Box<dyn Error>> {
        match message {
            GroupMessage::Import {
                connection,
                surface_id,
                import,
                reply,
            } => {
                let imported = import(&mut self.compositor).map_err(|error| error.to_string());
                if imported.is_ok() {
                    self.surfaces.insert(connection, surface_id);
                    self.replan();
                }
                let _ = reply.send(imported);
            }
            GroupMessage::Frame(connection, update) => {
                Renderer::set_latest(&mut self.compositor, connection, update.frame);
                let _ = self.events.frames.send(SurfaceFrameEvent {
                    connection,
                    update,
                    observed_qpc: performance_counter().unwrap_or(0),
                });
            }
            GroupMessage::Remove(connection) => {
                self.surfaces.remove(&connection);
                Renderer::disconnect_stream(&mut self.compositor, connection)?;
                self.replan();
            }
            GroupMessage::Arrange(layout) => {
                if layout.extent != self.layout.extent && !layout.extent.is_empty() {
                    self.compositor.resize_to(layout.extent)?;
                }
                self.layout = layout;
                self.replan();
                self.remeasure();
            }
            GroupMessage::Shader(control) => self.control_shader(control)?,
            GroupMessage::Stop => {}
            GroupMessage::Snapshot(connection, reply) => {
                let snapshot = self
                    .compositor
                    .snapshot_stream_rgba(connection)
                    .map(|snapshot| {
                        snapshot.map(|(width, height, pixels)| polymer_runtime::RgbaScreenshot {
                            width,
                            height,
                            pixels,
                        })
                    })
                    .map_err(|error| error.to_string());
                let _ = reply.send(snapshot);
            }
        }
        Ok(())
    }

    fn control_shader(&mut self, control: ShaderControl) -> Result<(), Box<dyn Error>> {
        match control {
            ShaderControl::Attach(shader) => self.build_shader(*shader)?,
            ShaderControl::Detach => {
                self.shader = ShaderState {
                    generation: self.shader.generation + 1,
                    ..ShaderState::default()
                };
                self.compositor.set_shader(None)?;
                self.measure_due = None;
                let _ = self.events.shaders.send(ShaderContent {
                    presenter: self.id,
                    content: None,
                });
            }
            ShaderControl::Parameter(name, value) => {
                if let Some(stage) = self.compositor.shader() {
                    stage.set_parameter(&name, value);
                }
                self.shader
                    .parameters
                    .retain(|(existing, _)| *existing != name);
                self.shader.parameters.push((name, value));
                self.remeasure();
            }
            ShaderControl::Passes(passes) => {
                if let Some(stage) = self.compositor.shader() {
                    stage.set_passes_enabled(passes);
                }
                self.shader.passes = Some(passes);
                self.remeasure();
            }
        }
        self.stale = true;
        Ok(())
    }

    fn build_shader(&mut self, shader: SurfaceShader) -> Result<(), Box<dyn Error>> {
        self.shader.generation += 1;
        self.shader.parameters = shader.parameters;
        self.shader.passes = shader.passes;
        let (sender, _) = self.shader.built.get_or_insert_with(mpsc::channel);
        let sender = sender.clone();
        let generation = self.shader.generation;
        let device = self.compositor.d3d12_device().clone();
        let (pack, viewport, aspect_ratio) = (shader.pack, shader.viewport, shader.aspect_ratio);
        thread::Builder::new()
            .name("polymer-shader-build".to_owned())
            .spawn(move || {
                let built = ShaderStage::build(&device, pack, viewport, aspect_ratio);
                let _ = sender.send((generation, built));
            })?;
        Ok(())
    }

    fn install_built_shader(&mut self) -> Result<(), Box<dyn Error>> {
        let Some((_, built)) = &self.shader.built else {
            return Ok(());
        };
        let Ok((generation, built)) = built.try_recv() else {
            return Ok(());
        };
        if generation != self.shader.generation {
            return Ok(());
        }
        let stage = match built {
            Ok(stage) => stage,
            Err(error) => {
                tracing::error!("polymer: compiling a shader preset failed: {error}");
                return Ok(());
            }
        };
        for (name, value) in &self.shader.parameters {
            stage.set_parameter(name, *value);
        }
        if let Some(passes) = self.shader.passes {
            stage.set_passes_enabled(passes);
        }
        self.compositor.set_shader(Some(stage))?;
        self.stale = true;
        self.remeasure();
        Ok(())
    }

    fn replan(&mut self) {
        let placements = self
            .layout
            .members
            .iter()
            .filter_map(|member| {
                let surface_id = *self.surfaces.get(&member.connection)?;
                let mut placement = SurfacePlacement::new(
                    member.connection,
                    surface_id,
                    member.rectangle,
                    member.corner_radius,
                );
                placement.content = member.placement;
                Some(placement)
            })
            .collect();
        self.plan = CompositingPlan::new([0.0; 4], placements);
        self.stale = true;
    }

    fn redraw_due(&self) -> bool {
        self.stale
            || Renderer::has_pending_frame(&self.compositor)
            || self.compositor.shader().is_some_and(ShaderStage::settling)
    }

    fn present(&mut self) -> Result<(), Box<dyn Error>> {
        if !self.redraw_due() || !self.compositor.begin_frame_within(PERMIT_WAIT) {
            return Ok(());
        }
        self.stale = false;
        self.compositor.present(&self.plan)?;
        Ok(())
    }
}
