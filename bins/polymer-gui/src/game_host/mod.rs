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

mod commands;
mod geometry;
mod launch;
mod surfaces;

pub(crate) use geometry::*;

/// Logical capture placement and input metadata; GPU resources belong to the compositor.
#[derive(Clone)]
struct CapturedSurface {
    index: u32,
    preferred_width: u32,
    preferred_height: u32,
    quarter_turns: u8,
    geometries: Vec<SurfaceGeometry>,
}

impl CapturedSurface {
    fn primary(&self) -> &SurfaceGeometry {
        &self.geometries[0]
    }
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
            let source = Extent::new(
                surface.preferred_width.max(1),
                surface.preferred_height.max(1),
            );
            for (instance, geometry) in surface.geometries.iter().enumerate() {
                let rectangle = Rect::new(geometry.x, geometry.y, geometry.width, geometry.height);
                snapshots.push(CapturedSurfaceSnapshot {
                    connection,
                    instance: instance as u32,
                    parent: geometry.group,
                    z_order: geometry.z_index,
                    placement: fit_surface_content(
                        rectangle,
                        source,
                        geometry.style.as_ref(),
                        geometry.opacity,
                    ),
                });
            }
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
            .filter(|(_, surface)| surface.primary().pointer_events)
            .collect::<Vec<_>>();
        surfaces.sort_by_key(|(_, surface)| std::cmp::Reverse(surface.primary().z_index));
        for (&connection, surface) in surfaces {
            let received_surfaces = self.compositor.borrow().received_surfaces(connection);
            for mut received_surface in received_surfaces {
                received_surface.frame.presentation_transform = normalize_pointer_transform(
                    received_surface.frame.presentation_transform,
                    received_surface.frame.presentation_width,
                    received_surface.frame.presentation_height,
                );
                for geometry in &surface.geometries {
                    let rectangle =
                        Rect::new(geometry.x, geometry.y, geometry.width, geometry.height);
                    let mut placement = SurfacePlacement::new(
                        connection,
                        received_surface.surface_id,
                        rectangle,
                        geometry.corner_radius,
                    );
                    placement.content = fit_surface_content(
                        rectangle,
                        received_surface.image_extent,
                        geometry.style.as_ref(),
                        geometry.opacity,
                    );
                    placements.push(placement);
                }
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
