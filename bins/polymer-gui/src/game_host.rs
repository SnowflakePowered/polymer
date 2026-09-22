//! Native emulator/capture state owned by the game-host subsystem.
//!
//! Validated launch-plan adaptation, process lifecycle, and captured-surface
//! models are contained here; the shell coordinator only forwards commands.

use crate::{
    launch::AttachedLaunchPlan, shell_document::GameViewStyle, shell_dom::TrackedSurfaceLayout,
};
use polymer_composition::{CompositorHandle, SurfaceLayer as CapturedSurfaceSnapshot};
use polymer_pluginhost::controller::AttachAutomation;
use polymer_pluginhost::controller::{
    AttachActions, ControllerChanged, DiscChange, PauseResume, PendingReply, Quit, StateAction,
};
use polymer_protocol::{PresentationMappingKind, PresentationTransform};
use polymer_renderer::layout::Rect;
use polymer_renderer::{
    CompositingPlan, Extent, SurfaceCapture, SurfaceCaptureEvent, SurfaceContent, SurfacePlacement,
    SurfacePointerSender,
};
use std::error::Error;
use std::time::Duration;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, atomic::AtomicBool},
};

/// Logical capture placement and input metadata; GPU resources belong to the compositor.
#[derive(Clone)]
struct CapturedSurface {
    index: u32,
    preferred_width: u32,
    preferred_height: u32,
    viewport_width: u32,
    viewport_height: u32,
    viewport_x: i32,
    viewport_y: i32,
    quarter_turns: u8,
    pointer_events: bool,
    z_index: i32,
    group: Option<u64>,
    corner_radius: f32,
    game_view_style: Option<GameViewStyle>,
    opacity: f32,
}

struct ActivePresentation {
    token: crate::shell_runtime::InstanceToken,
    options: AttachedLaunchPlan,
    webrtc: bool,
    controller_session: Option<polymer_pluginhost::controller::ControllerSession>,
    automation: Option<polymer_automation::AutomationSession>,
    save_states: Option<polymer_runtime::SaveStateManager>,
    current_disc_index: u32,
    closing: bool,
}

pub(crate) struct ConnectedSurface {
    pub connection: u64,
    pub surface_id: u64,
    pub target_index: u32,
    pub replaced_connection: Option<u64>,
    pub backend: String,
}

pub(crate) enum GameHostCaptureEvent {
    DeviceRequired,
    Connected(Option<ConnectedSurface>),
    Frame {
        connection: u64,
        frame: polymer_protocol::FrameReady,
        observed_qpc: Option<u64>,
        dimensions_changed: bool,
    },
    Removed {
        connection: u64,
        layout_changed: bool,
    },
    Rejected {
        connection: u64,
        error: String,
    },
}

pub(crate) struct FinishedGame {
    pub token: crate::shell_runtime::InstanceToken,
    pub connections: Vec<u64>,
    pub finalization_error: Option<String>,
}

pub(crate) struct GameSurfaceTarget {
    pub connection: u64,
    pub index: u32,
    pub width: u32,
    pub height: u32,
}

pub(crate) struct GameHost {
    active: Option<ActivePresentation>,
    session: polymer_runtime::SessionController,
    capture: SurfaceCapture,
    pointer: SurfacePointerSender,
    wake_pending: Arc<AtomicBool>,
    dirty: Arc<AtomicBool>,
    work_pending: bool,
    surfaces: HashMap<u64, CapturedSurface>,
    pending_offer: Option<polymer_runtime::SurfaceOffer>,
    /// Adapter-local capture device and imported captured streams.
    compositor: CompositorHandle,
    composition_revision: u64,
    shell_refresh_pending: bool,
}

impl GameHost {
    pub fn new(compositor: CompositorHandle) -> Result<Self, Box<dyn Error>> {
        let capture = SurfaceCapture::default();
        let pointer = capture.pointer_sender();
        Ok(Self {
            active: None,
            session: polymer_runtime::SessionController::default(),
            capture,
            pointer,
            wake_pending: Arc::new(AtomicBool::new(false)),
            dirty: Arc::new(AtomicBool::new(false)),
            work_pending: false,
            surfaces: HashMap::new(),
            pending_offer: None,
            compositor,
            composition_revision: 0,
            shell_refresh_pending: false,
        })
    }

    pub(crate) fn captured_surface_snapshots(&self) -> Vec<CapturedSurfaceSnapshot> {
        let mut snapshots = Vec::new();
        for (&connection, surface) in &self.surfaces {
            let rectangle = Rect::new(
                surface.viewport_x,
                surface.viewport_y,
                surface.viewport_width,
                surface.viewport_height,
            );
            let source = Extent::new(
                surface.preferred_width.max(1),
                surface.preferred_height.max(1),
            );
            let fitted = fit_surface_content(
                rectangle,
                source,
                surface.game_view_style.as_ref(),
                surface.opacity,
            );
            snapshots.push(CapturedSurfaceSnapshot {
                connection,
                parent: surface.group,
                z_order: surface.z_index,
                placement: fitted,
            });
        }
        snapshots.sort_by_key(|surface| surface.z_order);
        snapshots
    }

    pub(crate) const fn composition_revision(&self) -> u64 {
        self.composition_revision
    }

    fn mark_composition_dirty(&mut self) {
        self.composition_revision = self.composition_revision.wrapping_add(1);
    }

    pub fn is_active(&self, token: crate::shell_runtime::InstanceToken) -> bool {
        self.session.is_active(token.0)
    }

    pub fn active_token(&self) -> Option<crate::shell_runtime::InstanceToken> {
        self.session
            .active_token()
            .map(crate::shell_runtime::InstanceToken)
    }

    pub fn has_active_game(&self) -> bool {
        self.session.has_active_session()
    }

    pub fn controller_change(
        &self,
        change: polymer_controllers::ControllerTopologyChange,
    ) -> Result<Option<PendingReply<ControllerChanged>>, String> {
        let Some(active) = &self.active else {
            return Ok(None);
        };
        let Some(session) = &active.controller_session else {
            return Ok(None);
        };
        tracing::info!(
            token = active.token.0,
            previous = %change.previous.assignment_diagnostics(),
            requested = %change.requested.assignment_diagnostics(),
            "polymer: host queued controller topology for plugin"
        );
        Ok(Some(Box::pin(session.send(ControllerChanged(change)))))
    }

    pub fn pause_resume(
        &self,
        state: polymer_pluginhost::controller::PauseState,
        timeout: std::time::Duration,
    ) -> Result<Option<PendingReply<PauseResume>>, String> {
        let Some(active) = &self.active else {
            return Ok(None);
        };
        let Some(session) = &active.controller_session else {
            return Ok(None);
        };
        Ok(Some(Box::pin(session.send(PauseResume { state, timeout }))))
    }

    pub fn change_disc(&self, index: u32) -> Result<PendingReply<DiscChange>, String> {
        let active = self.active.as_ref().ok_or("emulator is no longer active")?;
        if active.closing {
            return Err("emulator is closing".to_owned());
        }
        if !active.options.can_change_disc || active.options.launch_set.len() < 2 {
            return Err("emulator does not support disc changes".to_owned());
        }
        if index as usize >= active.options.launch_set.len() {
            return Err("disc index is outside the launch set".to_owned());
        }
        let session = active
            .controller_session
            .as_ref()
            .ok_or("emulator plugin session is unavailable")?;
        Ok(Box::pin(session.send(DiscChange {
            index,
            timeout: Duration::from_secs(30),
        })))
    }

    pub fn commit_disc_change(&mut self, index: u32) -> Result<(), String> {
        let active = self.active.as_mut().ok_or("emulator is no longer active")?;
        active.current_disc_index = index;
        Ok(())
    }

    pub fn save_state(
        &self,
        timeout: std::time::Duration,
    ) -> Result<(u32, PendingReply<StateAction>), String> {
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| "emulator is no longer active".to_owned())?;
        if active.closing {
            return Err("emulator is closing".to_owned());
        }
        let states = active
            .save_states
            .as_ref()
            .ok_or_else(|| "emulator does not support save states".to_owned())?;
        let session = active
            .controller_session
            .as_ref()
            .ok_or_else(|| "emulator plugin session is unavailable".to_owned())?;
        let slot = states.next_slot();
        Ok((
            slot,
            Box::pin(session.send(StateAction {
                slot,
                save: true,
                timeout,
            })),
        ))
    }

    pub fn resume_state(
        &self,
        slot: u32,
        timeout: std::time::Duration,
    ) -> Result<PendingReply<StateAction>, String> {
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| "emulator is no longer active".to_owned())?;
        if active.closing {
            return Err("emulator is closing".to_owned());
        }
        let states = active
            .save_states
            .as_ref()
            .ok_or_else(|| "emulator does not support save states".to_owned())?;
        if !states.contains(slot) {
            return Err(format!("save-state slot {slot} does not exist"));
        }
        let session = active
            .controller_session
            .as_ref()
            .ok_or_else(|| "emulator plugin session is unavailable".to_owned())?;
        Ok(Box::pin(session.send(StateAction {
            slot,
            save: false,
            timeout,
        })))
    }

    pub fn commit_save_state(
        &mut self,
        slot: u32,
    ) -> Result<polymer_runtime::SaveStateMetadata, String> {
        let screenshot = self.capture_primary_screenshot();
        let states = self
            .active
            .as_mut()
            .and_then(|active| active.save_states.as_mut())
            .ok_or_else(|| "emulator does not support save states".to_owned())?;
        Ok(states.commit(slot, std::time::SystemTime::now(), screenshot))
    }

    pub fn capture_primary_screenshot(&mut self) -> Option<polymer_runtime::RgbaScreenshot> {
        let connection = self
            .surfaces
            .iter()
            .find_map(|(&connection, surface)| (surface.index == 0).then_some(connection))?;
        let captured = self.compositor.borrow_mut().snapshot(connection);
        match captured {
            Ok(screenshot) => screenshot,
            Err(error) => {
                tracing::warn!("polymer: primary-surface screenshot failed: {error}");
                None
            }
        }
    }

    pub fn save_state_metadata(&self, slot: u32) -> Option<polymer_runtime::SaveStateMetadata> {
        self.active
            .as_ref()?
            .save_states
            .as_ref()?
            .metadata_for_slot(slot)
    }

    pub fn save_state_screenshot(&self, record_id: &str) -> Result<Option<Vec<u8>>, String> {
        let states = self
            .active
            .as_ref()
            .and_then(|active| active.save_states.as_ref())
            .ok_or_else(|| "emulator does not support save states".to_owned())?;
        Ok(states.screenshot(record_id).map(<[u8]>::to_vec))
    }

    pub fn begin_quit(
        &mut self,
        token: crate::shell_runtime::InstanceToken,
        timeout: std::time::Duration,
    ) -> Result<
        Option<(
            crate::launch::QuickResumeCache,
            Option<Vec<u8>>,
            PendingReply<Quit>,
        )>,
        String,
    > {
        let cache = {
            let Some(active) = self.active.as_mut().filter(|active| active.token == token) else {
                return Ok(None);
            };
            if active.closing {
                return Err("emulator is already closing".to_owned());
            }
            active.closing = true;
            active.options.quick_resume_cache.clone().map(|mut cache| {
                cache.disc_index = active.current_disc_index;
                cache
            })
        };
        let Some(cache) = cache else {
            return Ok(None);
        };
        let screenshot = self
            .capture_primary_screenshot()
            .and_then(|screenshot| match screenshot.encode_png() {
                Ok(png) => Some(png),
                Err(error) => {
                    tracing::warn!("polymer: Quick Resume screenshot encoding failed: {error}");
                    None
                }
            });
        let Some(session) = self
            .active
            .as_ref()
            .and_then(|active| active.controller_session.as_ref())
        else {
            return Err("Quick Resume plugin session is unavailable".to_owned());
        };
        Ok(Some((
            cache,
            screenshot,
            Box::pin(session.send(Quit { timeout })),
        )))
    }

    pub fn surface_targets(&self) -> Vec<GameSurfaceTarget> {
        self.surfaces
            .iter()
            .map(|(&connection, surface)| GameSurfaceTarget {
                connection,
                index: surface.index,
                width: surface.preferred_width,
                height: surface.preferred_height,
            })
            .collect()
    }

    pub fn pointer_sender(&self) -> &SurfacePointerSender {
        &self.pointer
    }

    pub fn pointer_presentation(
        &self,
        extent: Extent,
    ) -> (polymer_renderer::Presentation, CompositingPlan) {
        let mut received = Vec::new();
        let mut placements = Vec::new();
        let mut surfaces = self
            .surfaces
            .iter()
            .filter(|(_, surface)| surface.pointer_events)
            .collect::<Vec<_>>();
        surfaces.sort_by_key(|(_, surface)| std::cmp::Reverse(surface.z_index));
        for (&connection, surface) in surfaces {
            let received_surfaces = self.compositor.borrow().received_surfaces(connection);
            for mut received_surface in received_surfaces {
                received_surface.frame.presentation_transform = normalize_pointer_transform(
                    received_surface.frame.presentation_transform,
                    received_surface.frame.presentation_width,
                    received_surface.frame.presentation_height,
                );
                let rectangle = Rect::new(
                    surface.viewport_x,
                    surface.viewport_y,
                    surface.viewport_width,
                    surface.viewport_height,
                );
                let mut placement = SurfacePlacement::new(
                    connection,
                    received_surface.surface_id,
                    rectangle,
                    surface.corner_radius,
                );
                placement.content = fit_surface_content(
                    rectangle,
                    received_surface.image_extent,
                    surface.game_view_style.as_ref(),
                    surface.opacity,
                );
                placements.push(placement);
                received.push(received_surface);
            }
        }
        (
            polymer_renderer::Presentation::new(extent, received),
            CompositingPlan::new([0.0; 4], placements),
        )
    }

    pub fn begin_capture_drain(&self) {
        self.dirty
            .store(false, std::sync::atomic::Ordering::Release);
    }

    pub fn finish_capture_drain(&mut self, exhausted_turn_budget: bool) {
        self.wake_pending
            .store(false, std::sync::atomic::Ordering::Release);
        let arrived_while_draining = self.dirty.swap(false, std::sync::atomic::Ordering::AcqRel);
        self.work_pending = exhausted_turn_budget || arrived_while_draining;
    }

    pub const fn has_capture_work(&self) -> bool {
        self.work_pending
    }

    pub fn take_shell_refresh_request(&mut self) -> bool {
        std::mem::take(&mut self.shell_refresh_pending)
    }

    pub fn clear_shell_refresh_request(&mut self) {
        self.shell_refresh_pending = false;
    }

    pub fn launch(
        &mut self,
        token: crate::shell_runtime::InstanceToken,
        options: AttachedLaunchPlan,
        filesystem: polymer_vfs::MountedFilesystem,
        controller_session: Option<polymer_pluginhost::controller::ControllerSession>,
        session: String,
        wake_window: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Result<(), String> {
        if self.session.has_active_session() {
            return Err("another emulator instance is active".to_owned());
        }
        let artifact_root = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
            .ok_or_else(|| "shell executable has no parent directory".to_owned())?;
        #[cfg(windows)]
        let automation_desktop = options.isolated.then(|| session.clone());
        let config = polymer_runtime::EmulatorLaunchConfig {
            session,
            command: options.command.clone(),
            working_directory: options.working_directory.as_ref().map(Into::into),
            env: options.env.clone(),
            arguments: options.arguments.clone(),
            artifact_root,
            hooks: options.hooks.clone(),
            consumer: self.compositor.borrow().consumer_preference(),
            isolate: options.isolated,
            tracked_surfaces: options
                .surfaces
                .iter()
                .map(|surface| polymer_runtime::TrackedSurfaceConfig {
                    id: polymer_runtime::TrackedSurfaceId(surface.index),
                    render_target_index: surface.index,
                    window_pattern: surface.selector.clone(),
                    rotation: surface.rotation,
                })
                .collect(),
            tracking_enabled: !options.surfaces.is_empty(),
            skip_surfaces: 0,
        };
        let dirty = self.dirty.clone();
        let wake_pending = self.wake_pending.clone();
        self.session.launch_with_input(
            token.0,
            config,
            filesystem,
            move || {
                dirty.store(true, std::sync::atomic::Ordering::Release);
                if !wake_pending.swap(true, std::sync::atomic::Ordering::AcqRel) && !wake_window() {
                    wake_pending.store(false, std::sync::atomic::Ordering::Release);
                }
            },
            move |_listener| Ok(()),
        )?;
        if let Some(controller_session) = &controller_session {
            let endpoint = self
                .session
                .instance()
                .expect("a successful session launch owns an emulator instance")
                .action_endpoint();
            let attached = futures::executor::block_on(controller_session.send(AttachActions(
                std::sync::Arc::new(move |hotkey| match hotkey {
                    polymer_pluginhost::actions::ParsedHotkey::Keyboard { code, modifiers } => {
                        endpoint.activate_keyboard(code, modifiers)
                    }
                    polymer_pluginhost::actions::ParsedHotkey::Binding { player, keys } => {
                        endpoint.activate_bindings(*player, keys)
                    }
                }),
            )))
            .map_err(|error| error.to_string())
            .and_then(|result| result);
            if let Err(error) = attached {
                let _ = self
                    .session
                    .finish(polymer_vfs::FinalizationReason::LaunchFailed);
                return Err(error);
            }
        }
        let automation = if let Some(controller_session) = &controller_session {
            let process_id = self
                .session
                .instance()
                .expect("a successful session launch owns an emulator instance")
                .process_id();
            #[cfg(windows)]
            let driver = polymer_platform_windows::WindowsAutomationDriver::new(
                process_id,
                automation_desktop,
            );
            #[cfg(target_os = "macos")]
            let driver = polymer_platform_macos::MacAutomationDriver::new(
                process_id,
                self.session.instance().unwrap().input(),
            );
            let attached = driver
                .and_then(polymer_automation::AutomationSession::spawn)
                .map_err(|error| format!("could not start emulator UI automation: {error}"))
                .and_then(|automation| {
                    futures::executor::block_on(
                        controller_session.send(AttachAutomation(automation.client())),
                    )
                    .map_err(|error| error.to_string())??;
                    Ok(automation)
                });
            match attached {
                Ok(automation) => Some(automation),
                Err(error) => {
                    let _ = self
                        .session
                        .finish(polymer_vfs::FinalizationReason::LaunchFailed);
                    return Err(error);
                }
            }
        } else {
            None
        };
        self.capture.attach(
            self.session
                .instance()
                .expect("a successful session launch owns an emulator instance"),
        );
        let save_states = options
            .save_states
            .as_ref()
            .map(|states| polymer_runtime::SaveStateManager::new(states.max));
        self.active = Some(ActivePresentation {
            token,
            current_disc_index: options.initial_disc_index,
            options,
            webrtc: false,
            controller_session,
            automation,
            save_states,
            closing: false,
        });
        Ok(())
    }

    pub fn start_webrtc(
        &mut self,
        token: crate::shell_runtime::InstanceToken,
        bind: std::net::SocketAddr,
        targets: &[u32],
    ) -> Result<(), String> {
        let Some(active) = self.active.as_mut().filter(|active| active.token == token) else {
            return Err("the emulator instance is not active".to_owned());
        };
        if active.webrtc {
            return Err("the emulator instance already has a WebRTC publication".to_owned());
        }
        if let Some(target) = targets.iter().find(|target| {
            !active
                .options
                .surfaces
                .iter()
                .any(|surface| surface.index == **target)
        }) {
            return Err(format!(
                "WebRTC render target {target} is not part of the emulator launch"
            ));
        }
        let streams = self
            .surfaces
            .iter()
            .map(|(&connection, surface)| (connection, surface.index))
            .collect::<Vec<_>>();
        self.compositor
            .borrow_mut()
            .start_webrtc(bind, targets, &streams)
            .map_err(|error| error.to_string())?;
        active.webrtc = true;
        Ok(())
    }

    pub fn stop_webrtc(&mut self, token: crate::shell_runtime::InstanceToken) -> bool {
        let Some(active) = self.active.as_mut().filter(|active| active.token == token) else {
            return false;
        };
        let active_publication = std::mem::replace(&mut active.webrtc, false);
        self.compositor.borrow_mut().stop_webrtc();
        active_publication
    }

    pub fn input_endpoint(&self) -> Option<polymer_ipc::input::host::InputIpcListener> {
        self.session.instance().map(|instance| instance.input())
    }

    pub fn request_close(&mut self, token: crate::shell_runtime::InstanceToken) -> bool {
        if !self.session.close(token.0) {
            return false;
        }
        self.capture.detach();
        true
    }

    pub fn set_volume(&self, token: crate::shell_runtime::InstanceToken, volume: f64) -> bool {
        self.session.set_volume(token.0, volume)
    }

    pub fn set_muted(&self, token: crate::shell_runtime::InstanceToken, muted: bool) -> bool {
        self.session.set_muted(token.0, muted)
    }

    pub fn audio_change(
        &mut self,
        force: bool,
    ) -> Option<(
        crate::shell_runtime::InstanceToken,
        polymer_runtime::AudioState,
    )> {
        self.session
            .audio_change(force)
            .map(|(token, audio)| (crate::shell_runtime::InstanceToken(token), audio))
    }

    pub fn has_exited(&self) -> bool {
        self.session.has_exited()
    }

    pub fn detach_capture(&mut self) {
        self.capture.detach();
    }

    /// Consumes and applies one capture event. Raw runtime offers and frame
    /// updates never leave the game-host boundary.
    pub fn try_capture_event(&mut self) -> Option<GameHostCaptureEvent> {
        if let Some(offer) = self.pending_offer.take() {
            return Some(self.handle_surface_offer(offer));
        }
        loop {
            let worker = self.compositor.borrow_mut().try_surface_event();
            let Some(worker) = worker else {
                break;
            };
            if let Some((frame, dimensions_changed)) =
                self.update_surface_frame(worker.connection, worker.update)
            {
                return Some(GameHostCaptureEvent::Frame {
                    connection: worker.connection,
                    frame,
                    observed_qpc: Some(worker.observed_qpc),
                    dimensions_changed,
                });
            }
        }
        loop {
            return Some(match self.capture.try_recv().ok()? {
                SurfaceCaptureEvent::Offered(offer) => self.handle_surface_offer(offer),
                SurfaceCaptureEvent::Frame { connection, update } => {
                    let Some((frame, dimensions_changed)) =
                        self.update_surface_frame(connection, update)
                    else {
                        continue;
                    };
                    GameHostCaptureEvent::Frame {
                        connection,
                        frame,
                        observed_qpc: None,
                        dimensions_changed,
                    }
                }
                SurfaceCaptureEvent::Removed { connection, .. } => GameHostCaptureEvent::Removed {
                    connection,
                    layout_changed: self.remove_surface(connection),
                },
            });
        }
    }

    fn handle_surface_offer(
        &mut self,
        offer: polymer_runtime::SurfaceOffer,
    ) -> GameHostCaptureEvent {
        if !self.compositor.borrow().matches_device(&offer) {
            self.pending_offer = Some(offer);
            return GameHostCaptureEvent::DeviceRequired;
        }
        let connection = offer.connection_id();
        match self.accept_surface_offer(offer) {
            Ok(accepted) => GameHostCaptureEvent::Connected(accepted),
            Err(error) => GameHostCaptureEvent::Rejected {
                connection,
                error: error.to_string(),
            },
        }
    }

    /// Selects the offer's adapter, imports it into the window-spanning
    /// compositor, and accepts the capture stream as one atomic host action.
    fn accept_surface_offer(
        &mut self,
        offer: polymer_runtime::SurfaceOffer,
    ) -> Result<Option<ConnectedSurface>, Box<dyn Error>> {
        let Some(surface_capacity) = self
            .active
            .as_ref()
            .map(|active| active.options.surfaces.len())
        else {
            self.capture.reject(offer);
            return Ok(None);
        };
        let connection = offer.connection_id();
        let surface_id = offer.stream().surface_id.0;
        let replaced_connection = offer
            .replaced_connection_id()
            .filter(|connection| self.surfaces.contains_key(connection));
        if replaced_connection.is_none() && self.surfaces.len() >= surface_capacity {
            self.capture.reject(offer);
            return Ok(None);
        }

        let (surface, target_index, quarter_turns) = if let Some(previous) = replaced_connection {
            let surface = self
                .surfaces
                .get(&previous)
                .cloned()
                .ok_or("the replaced tracked surface is no longer connected")?;
            let target_index = surface.index;
            let quarter_turns = surface.quarter_turns;
            (surface, target_index, quarter_turns)
        } else {
            let (target_index, quarter_turns) = self
                .next_target_configuration()
                .ok_or("the game host already has every requested render target")?;
            let stream = offer.stream();
            let (width, height) = rotated_extent(stream.width, stream.height, quarter_turns);
            (
                CapturedSurface {
                    index: target_index,
                    preferred_width: width,
                    preferred_height: height,
                    viewport_width: width.max(1),
                    viewport_height: height.max(1),
                    viewport_x: 0,
                    viewport_y: 0,
                    quarter_turns,
                    pointer_events: true,
                    z_index: target_index as i32,
                    group: None,
                    corner_radius: 0.0,
                    game_view_style: None,
                    opacity: 1.0,
                },
                target_index,
                quarter_turns,
            )
        };
        let backend = format!("{:?}", offer.stream().image_kind);
        self.compositor
            .borrow_mut()
            .import_surface(&offer, quarter_turns)?;
        let detached = self.compositor.borrow().accepts_detached_frames();
        let accepted = if detached {
            self.capture
                .accept_detached(offer)
                .map(|(_, frames)| Some(frames))
        } else {
            self.capture.accept(offer).map(|_| None)
        };
        let frames = match accepted {
            Ok(frames) => frames,
            Err(error) => {
                self.compositor.borrow_mut().remove_surface(connection);
                return Err(error.into());
            }
        };
        let registration = {
            let mut compositor = self.compositor.borrow_mut();
            let worker = if let Some(frames) = frames {
                compositor.attach_surface_frames(connection, frames, self.pointer.clone())
            } else {
                Ok(())
            };
            worker.and_then(|()| compositor.register_surface(connection, target_index))
        };
        if let Err(error) = registration {
            self.compositor.borrow_mut().remove_surface(connection);
            self.capture.revoke(connection);
            return Err(error);
        }
        self.surfaces.insert(connection, surface);
        if let Some(previous) = replaced_connection {
            self.surfaces.remove(&previous);
            self.compositor.borrow_mut().remove_surface(previous);
        }
        self.mark_composition_dirty();
        Ok(Some(ConnectedSurface {
            connection,
            surface_id,
            target_index,
            replaced_connection,
            backend,
        }))
    }

    pub(crate) fn replace_render_device(&mut self) -> Result<Vec<u64>, Box<dyn Error>> {
        let offer = self
            .pending_offer
            .as_ref()
            .ok_or("there is no pending surface offer")?;
        self.compositor.borrow_mut().replace_device(offer)?;
        let stale_connections = self.surfaces.keys().copied().collect::<Vec<_>>();
        for &connection in &stale_connections {
            self.capture.revoke(connection);
        }
        self.surfaces.clear();
        self.mark_composition_dirty();
        self.shell_refresh_pending = true;
        Ok(stale_connections)
    }

    pub(crate) fn reject_pending_offer(&mut self) -> Option<u64> {
        let offer = self.pending_offer.take()?;
        let connection = offer.connection_id();
        self.capture.reject(offer);
        Some(connection)
    }

    fn update_surface_frame(
        &mut self,
        connection: u64,
        update: polymer_runtime::SurfaceFrameUpdate,
    ) -> Option<(polymer_protocol::FrameReady, bool)> {
        let frame = update.frame;
        let surface = self.surfaces.get_mut(&connection)?;
        let (width, height) = rotated_extent(
            frame.render_width.max(1),
            frame.render_height.max(1),
            surface.quarter_turns,
        );
        let dimensions_changed =
            (surface.preferred_width, surface.preferred_height) != (width, height);
        if dimensions_changed {
            surface.preferred_width = width;
            surface.preferred_height = height;
        }
        self.compositor.borrow_mut().set_latest(connection, frame);
        if dimensions_changed {
            self.mark_composition_dirty();
        }
        Some((frame, dimensions_changed))
    }

    fn remove_surface(&mut self, connection: u64) -> bool {
        self.compositor.borrow_mut().remove_surface(connection);
        let removed = self.surfaces.remove(&connection).is_some();
        if removed {
            self.mark_composition_dirty();
        }
        removed
    }

    pub fn finish_active_game(&mut self) -> Option<FinishedGame> {
        let mut active = self.active.take()?;
        let connections = self.surfaces.keys().copied().collect::<Vec<_>>();
        self.compositor.borrow_mut().stop_webrtc();
        for &connection in &connections {
            self.compositor.borrow_mut().remove_surface(connection);
            self.capture.revoke(connection);
        }
        self.capture.detach();
        self.surfaces.clear();
        self.pending_offer = None;
        self.mark_composition_dirty();
        let token = active.token;
        if let Some(automation) = active.automation.take() {
            automation.close();
        }
        drop(active.controller_session.take());
        let finalization_error = self
            .session
            .finish(polymer_vfs::FinalizationReason::ProcessExited)
            .err();
        Some(FinishedGame {
            token,
            connections,
            finalization_error,
        })
    }

    fn next_target_configuration(&self) -> Option<(u32, u8)> {
        let active = self.active.as_ref()?;
        active
            .options
            .surfaces
            .iter()
            .find(|target| {
                !self
                    .surfaces
                    .values()
                    .any(|surface| surface.index == target.index)
            })
            .map(|target| (target.index, normalized_quarter_turns(target.rotation)))
    }

    pub fn has_pending_frame(&self) -> bool {
        self.compositor.borrow().has_pending_frame()
    }

    pub fn draw(
        &mut self,
        _width: u32,
        _height: u32,
        permit_wait: Duration,
    ) -> Result<Option<bool>, String> {
        let corners = self
            .surfaces
            .iter()
            .map(|(&connection, surface)| (connection, surface.corner_radius))
            .collect::<Vec<_>>();
        self.compositor
            .borrow_mut()
            .present_surfaces(&corners, permit_wait)
    }

    pub fn apply_tracked_layout(&mut self, placements: &[TrackedSurfaceLayout]) -> bool {
        let mut placed = HashSet::new();
        let mut changed = false;
        for placement in placements {
            let Some((&connection, surface)) = self
                .surfaces
                .iter_mut()
                .find(|(_, surface)| u64::from(surface.index) == placement.source_id)
            else {
                continue;
            };
            // Keep the guest visual in the tracked element's full coordinate
            // space. Shrinking this rectangle to the accumulated paint clip
            // changes object-fit and relocates the sampled image whenever a
            // panel approaches a viewport edge, while its chrome remains in
            // the original space. The desktop composition root already clips
            // visuals at the window boundary; finer clips belong on the WUC
            // visual itself, not in its layout geometry.
            let rect = placement.bounds;
            let next = SurfaceGeometry {
                x: rect.x0.round() as i32,
                y: rect.y0.round() as i32,
                width: rect.width().round().max(0.0) as u32,
                height: rect.height().round().max(0.0) as u32,
                pointer_events: true,
                z_index: placement.z_order,
                group: placement.group,
                corner_radius: placement.corner_radius,
                style: placement.style.clone(),
                opacity: placement.opacity,
            };
            changed |= apply_geometry(surface, next);
            placed.insert(connection);
        }

        let requested = self
            .active
            .as_ref()
            .map(|active| {
                active
                    .options
                    .surfaces
                    .iter()
                    .map(|surface| surface.index)
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_default();
        for (&connection, surface) in &mut self.surfaces {
            if placed.contains(&connection) || !requested.contains(&surface.index) {
                continue;
            }
            changed |= apply_geometry(
                surface,
                SurfaceGeometry {
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 0,
                    pointer_events: false,
                    z_index: 1,
                    group: None,
                    corner_radius: 0.0,
                    style: None,
                    opacity: 1.0,
                },
            );
        }
        if changed {
            self.mark_composition_dirty();
        }
        changed
    }
}

struct SurfaceGeometry {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    pointer_events: bool,
    z_index: i32,
    group: Option<u64>,
    corner_radius: f32,
    style: Option<GameViewStyle>,
    opacity: f32,
}

fn apply_geometry(surface: &mut CapturedSurface, next: SurfaceGeometry) -> bool {
    let changed = surface.viewport_x != next.x
        || surface.viewport_y != next.y
        || surface.viewport_width != next.width
        || surface.viewport_height != next.height
        || surface.pointer_events != next.pointer_events
        || surface.z_index != next.z_index
        || surface.group != next.group
        || surface.corner_radius != next.corner_radius
        || surface.game_view_style != next.style
        || surface.opacity != next.opacity;
    if changed {
        surface.viewport_x = next.x;
        surface.viewport_y = next.y;
        surface.viewport_width = next.width;
        surface.viewport_height = next.height;
        surface.pointer_events = next.pointer_events;
        surface.z_index = next.z_index;
        surface.group = next.group;
        surface.corner_radius = next.corner_radius;
        surface.game_view_style = next.style;
        surface.opacity = next.opacity;
    }
    changed
}

impl Drop for GameHost {
    fn drop(&mut self) {
        for &connection in self.surfaces.keys() {
            self.capture.revoke(connection);
        }
        if let Some(token) = self.session.active_token() {
            self.session.close(token);
        }
    }
}

/// Resolves CSS replaced-element sizing without changing the layout rectangle
/// used for guest resizing and input. Overflow becomes a source crop.
pub(crate) fn fit_surface_content(
    rectangle: Rect,
    source: Extent,
    style: Option<&GameViewStyle>,
    opacity: f32,
) -> SurfaceContent {
    use style::{
        properties::generated::longhands::object_fit::computed_value::T as ObjectFit,
        values::computed::CSSPixelLength,
    };

    let mut content = SurfaceContent::fill(rectangle);
    content.opacity = if opacity.is_finite() {
        opacity.clamp(0.0, 1.0)
    } else {
        1.0
    };
    if rectangle.is_empty() || source.is_empty() {
        return content;
    }
    let container_width = rectangle.width as f32;
    let container_height = rectangle.height as f32;
    let source_width = source.width as f32;
    let source_height = source.height as f32;
    let Some(style) = style else {
        return content;
    };
    let contain_scale = (container_width / source_width).min(container_height / source_height);
    let cover_scale = (container_width / source_width).max(container_height / source_height);
    let (object_width, object_height) = match style.object_fit {
        ObjectFit::Fill => (container_width, container_height),
        ObjectFit::Contain => (source_width * contain_scale, source_height * contain_scale),
        ObjectFit::Cover => (source_width * cover_scale, source_height * cover_scale),
        ObjectFit::None => (source_width, source_height),
        ObjectFit::ScaleDown
            if source_width <= container_width && source_height <= container_height =>
        {
            (source_width, source_height)
        }
        ObjectFit::ScaleDown => (source_width * contain_scale, source_height * contain_scale),
    };
    let x_offset = style
        .object_position
        .horizontal
        .resolve(CSSPixelLength::new(container_width - object_width))
        .px();
    let y_offset = style
        .object_position
        .vertical
        .resolve(CSSPixelLength::new(container_height - object_height))
        .px();
    let container_left = rectangle.x as f32;
    let container_top = rectangle.y as f32;
    let container_right = container_left + container_width;
    let container_bottom = container_top + container_height;
    let object_left = container_left + x_offset;
    let object_top = container_top + y_offset;
    let object_right = object_left + object_width;
    let object_bottom = object_top + object_height;
    let draw_left = object_left.max(container_left).min(container_right);
    let draw_top = object_top.max(container_top).min(container_bottom);
    let draw_right = object_right.min(container_right).max(container_left);
    let draw_bottom = object_bottom.min(container_bottom).max(container_top);
    content.destination = [draw_left, draw_top, draw_right, draw_bottom];
    if draw_right > draw_left && draw_bottom > draw_top {
        content.source = [
            ((draw_left - object_left) / object_width).clamp(0.0, 1.0),
            ((draw_top - object_top) / object_height).clamp(0.0, 1.0),
            ((draw_right - object_left) / object_width).clamp(0.0, 1.0),
            ((draw_bottom - object_top) / object_height).clamp(0.0, 1.0),
        ];
    }
    content
}

pub(crate) const fn rotated_extent(width: u32, height: u32, quarter_turns: u8) -> (u32, u32) {
    if quarter_turns % 2 == 1 {
        (height, width)
    } else {
        (width, height)
    }
}

const fn normalized_quarter_turns(rotation: i32) -> u8 {
    rotation.rem_euclid(4) as u8
}

fn normalize_pointer_transform(
    transform: PresentationTransform,
    width: u32,
    height: u32,
) -> PresentationTransform {
    if transform.kind == PresentationMappingKind::Unavailable && width != 0 && height != 0 {
        PresentationTransform::axis_aligned(0, 0, width, height)
    } else {
        transform
    }
}
