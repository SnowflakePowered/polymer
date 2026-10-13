//! Native emulator/capture state owned by the game-host subsystem.
//!
//! Validated launch-plan adaptation, process lifecycle, and captured-surface
//! models are contained here; the shell coordinator only forwards commands.

use crate::{
    launch::AttachedLaunchPlan,
    shell::document::GameViewStyle,
    shell::protocol::{TrackedSource, TrackedSurfaceLayout},
};
use polymer_composition::{
    CompositorHandle, SurfaceGroupLayout, SurfaceGroupMember,
    SurfaceLayer as CapturedSurfaceSnapshot,
};
use polymer_pluginhost::controller::AttachAutomation;
use polymer_pluginhost::controller::{
    AttachActions, ControllerChanged, DiscChange, PauseResume, PendingReply, Quit, StateAction,
};
use polymer_renderer::layout::Rect;
use polymer_renderer::{
    CompositingPlan, Extent, ShaderViewport, SurfaceCapture, SurfaceCaptureEvent, SurfaceContent,
    SurfacePlacement, SurfacePointerSender,
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
mod shaders;
mod surfaces;
mod videos;

pub(crate) use geometry::*;

/// Logical capture placement and input metadata; GPU resources belong to the compositor.
#[derive(Clone)]
struct CapturedSurface {
    index: u32,
    preferred_width: u32,
    preferred_height: u32,
    quarter_turns: u8,
    /// Group whose swapchain this stream is stitched into, fixed at import.
    surface_group: Option<u64>,
    geometries: Vec<SurfaceGeometry>,
}

impl CapturedSurface {
    fn primary(&self) -> &SurfaceGeometry {
        &self.geometries[0]
    }

    fn source_extent(&self) -> Extent {
        Extent::new(self.preferred_width.max(1), self.preferred_height.max(1))
    }

    /// The box a shaded stream presented alone renders into: its fitted image
    /// for content presets, or its whole element for presets that draw borders.
    fn shaded_box(&self, geometry: &SurfaceGeometry, viewport: ShaderViewport) -> Rect {
        let rectangle = Rect::new(geometry.x, geometry.y, geometry.width, geometry.height);
        if viewport == ShaderViewport::Element {
            return rectangle;
        }
        let [left, top, right, bottom] = fit_surface_content(
            rectangle,
            self.source_extent(),
            geometry.style.as_ref(),
            geometry.opacity,
        )
        .destination;
        Rect::new(
            left.round() as i32,
            top.round() as i32,
            (right - left).round().max(0.0) as u32,
            (bottom - top).round().max(0.0) as u32,
        )
    }

    /// This stream's placements inside its group's box, in group-local pixels.
    /// A stream presented alone is its own box at its primary geometry.
    fn group_members(
        &self,
        connection: u64,
        alone: Option<ShaderViewport>,
    ) -> impl Iterator<Item = (i32, Extent, SurfaceGroupMember)> {
        let geometries = if alone.is_some() {
            &self.geometries[..1]
        } else {
            &self.geometries[..]
        };
        geometries.iter().filter_map(move |geometry| {
            let bounds = match alone {
                Some(viewport) => self.shaded_box(geometry, viewport),
                None => {
                    self.surface_group?;
                    geometry.surface_group?.bounds
                }
            };
            let rectangle = Rect::new(
                geometry.x - bounds.x,
                geometry.y - bounds.y,
                geometry.width,
                geometry.height,
            );
            let member = SurfaceGroupMember {
                connection,
                rectangle,
                corner_radius: geometry.corner_radius,
                placement: fit_surface_content(
                    rectangle,
                    self.source_extent(),
                    geometry.style.as_ref(),
                    geometry.opacity,
                ),
            };
            Some((geometry.z_index, bounds.extent(), member))
        })
    }
}

struct ActivePresentation {
    token: crate::shell::runtime::InstanceToken,
    options: AttachedLaunchPlan,
    controller_session: Option<polymer_pluginhost::controller::ControllerSession>,
    automation: Option<polymer_automation::AutomationSession>,
    current_disc_index: u32,
    closing: bool,
}

pub(crate) struct ConnectedSurface {
    pub connection: u64,
    pub surface_id: u64,
    pub target_index: u32,
    pub replaced_connection: Option<u64>,
    pub backend: String,
    pub extent: polymer_renderer::Extent,
}

pub(crate) enum GameHostCaptureEvent {
    DeviceRequired,
    Connected(Option<ConnectedSurface>),
    Frame {
        connection: u64,
        frame: polymer_protocol::FrameReady,
        observed_qpc: Option<u64>,
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
    pub token: crate::shell::runtime::InstanceToken,
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
    video_layout: HashMap<u64, Vec<SurfaceGeometry>>,
    video_extents: HashMap<u64, Extent>,
    surface_group_layouts: HashMap<u64, SurfaceGroupLayout>,
    shaders: HashMap<crate::shell::protocol::ShaderTarget, shaders::ShaderAttachment>,
    /// Where each shaded presenter's chain drew its composed surfaces, in box pixels.
    shader_contents: HashMap<u64, [f32; 4]>,
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
            video_layout: HashMap::new(),
            video_extents: HashMap::new(),
            surface_group_layouts: HashMap::new(),
            shaders: HashMap::new(),
            shader_contents: HashMap::new(),
            pending_offer: None,
            compositor,
            composition_revision: 0,
            shell_refresh_pending: false,
        })
    }

    pub(crate) fn captured_surface_snapshots(&self) -> Vec<CapturedSurfaceSnapshot> {
        let captured = self
            .surfaces
            .iter()
            .filter(|(_, surface)| surface.surface_group.is_none())
            .map(|(&connection, surface)| {
                (connection, surface.source_extent(), &surface.geometries)
            });
        let videos = self.video_layout.iter().map(|(&id, geometries)| {
            let source = self.video_extents.get(&id).copied();
            (id, source.unwrap_or(Extent::new(1, 1)), geometries)
        });
        let mut snapshots = Vec::new();
        for (connection, source, geometries) in captured.chain(videos) {
            let shaded = self
                .shaded_alone(connection)
                .zip(self.surfaces.get(&connection));
            for (instance, geometry) in geometries.iter().enumerate() {
                let rectangle = Rect::new(geometry.x, geometry.y, geometry.width, geometry.height);
                snapshots.push(CapturedSurfaceSnapshot {
                    connection,
                    instance: instance as u32,
                    parent: geometry.group,
                    z_order: geometry.z_index,
                    placement: if let Some((viewport, surface)) = shaded {
                        SurfaceContent::fill(surface.shaded_box(geometry, viewport))
                    } else {
                        fit_surface_content(
                            rectangle,
                            source,
                            geometry.style.as_ref(),
                            geometry.opacity,
                        )
                    },
                });
            }
        }
        snapshots.extend(self.surface_group_layers());
        snapshots.sort_by_key(|surface| surface.z_order);
        snapshots
    }

    /// One visual per surface group, below the chrome painted after its first member.
    fn surface_group_layers(&self) -> impl Iterator<Item = CapturedSurfaceSnapshot> {
        let mut layers = HashMap::<u64, CapturedSurfaceSnapshot>::new();
        for surface in self.surfaces.values() {
            let Some(group) = surface.surface_group else {
                continue;
            };
            for geometry in &surface.geometries {
                let Some(surface_group) = geometry.surface_group else {
                    continue;
                };
                let layer = layers.entry(group).or_insert(CapturedSurfaceSnapshot {
                    connection: group,
                    instance: 0,
                    parent: surface_group.parent,
                    z_order: geometry.z_index,
                    placement: SurfaceContent::fill(surface_group.bounds),
                });
                layer.z_order = layer.z_order.min(geometry.z_index);
            }
        }
        layers.into_values()
    }

    /// Sends each surface group, and each shaded stream presented alone, its
    /// stitched layout whenever it changes.
    fn arrange_surface_groups(&mut self) {
        let mut layouts = HashMap::<u64, (SurfaceGroupLayout, Vec<i32>)>::new();
        for (&connection, surface) in &self.surfaces {
            let alone = self.shaded_alone(connection);
            let Some(group) = surface.surface_group.or(alone.map(|_| connection)) else {
                continue;
            };
            let (layout, order) = layouts.entry(group).or_default();
            for (z_index, extent, member) in surface.group_members(connection, alone) {
                layout.extent = extent;
                let position = order.partition_point(|&other| other <= z_index);
                order.insert(position, z_index);
                layout.members.insert(position, member);
            }
        }
        self.surface_group_layouts
            .retain(|group, _| layouts.contains_key(group));
        for (group, (layout, _)) in layouts {
            if self.surface_group_layouts.get(&group) != Some(&layout) {
                self.compositor
                    .borrow_mut()
                    .arrange_surface_group(group, layout.clone());
                self.surface_group_layouts.insert(group, layout);
            }
        }
    }

    pub(crate) const fn composition_revision(&self) -> u64 {
        self.composition_revision
    }

    fn mark_composition_dirty(&mut self) {
        self.composition_revision = self.composition_revision.wrapping_add(1);
    }

    pub fn is_active(&self, token: crate::shell::runtime::InstanceToken) -> bool {
        self.session.is_active(token.0)
    }

    pub fn active_token(&self) -> Option<crate::shell::runtime::InstanceToken> {
        self.session
            .active_token()
            .map(crate::shell::runtime::InstanceToken)
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
            for received_surface in received_surfaces {
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
                    if let Some(destination) =
                        self.shaded_destination(connection, surface, geometry, &placement)
                    {
                        placement.content.destination = destination;
                        placement.rectangle = Rect::new(
                            destination[0].round() as i32,
                            destination[1].round() as i32,
                            (destination[2] - destination[0]).round().max(0.0) as u32,
                            (destination[3] - destination[1]).round().max(0.0) as u32,
                        );
                    }
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
