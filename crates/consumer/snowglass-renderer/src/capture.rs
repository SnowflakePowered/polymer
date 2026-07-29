//! Renderer-side consumption of runtime-managed emulator surfaces.

use crate::input::PointerEventSink;
use dashmap::DashMap;
use snowglass_input::mouse::PointerInput;
use snowglass_runtime::{
    EmulatorInstance, PresentedFrameToken, SurfaceError, SurfaceEvent, SurfaceEventReceiver,
    SurfaceFrameReceiver, SurfaceFrameUpdate, SurfaceOffer, SurfacePointerUpdate,
    TrackedSurfaceHandle, TrackedSurfaceId,
};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

const POINTER_FRAME_LIMIT: usize = 16;

struct PointerTarget {
    protocol_surface_id: u64,
    surface: TrackedSurfaceHandle,
    frames: VecDeque<(u64, PresentedFrameToken)>,
}

impl PointerTarget {
    fn remember(&mut self, update: SurfaceFrameUpdate) {
        self.frames
            .push_back((update.frame.frame_number, update.presentation_token()));
        while self.frames.len() > POINTER_FRAME_LIMIT {
            self.frames.pop_front();
        }
    }
}

/// Stable pointer endpoint for accepted surfaces.
///
/// Delivery goes back through [`TrackedSurfaceHandle`], which owns the current
/// target HWND and surface generation. Renderer metadata remains useful for
/// hit testing, but is not trusted as the input destination.
#[derive(Clone, Default)]
pub struct SurfacePointerSender {
    targets: Arc<DashMap<u64, PointerTarget>>,
}

impl PointerEventSink for SurfacePointerSender {
    fn send_pointer(&self, connection: u64, event: PointerInput) -> bool {
        let Some(target) = self.targets.get(&connection) else {
            return false;
        };
        if target.protocol_surface_id != event.surface_id {
            return false;
        }
        let Some(frame) = target
            .frames
            .iter()
            .rev()
            .find_map(|(number, token)| (*number == event.frame_number).then_some(*token))
        else {
            return false;
        };
        target
            .surface
            .send_pointer_update(
                frame,
                SurfacePointerUpdate {
                    source: event.source,
                    phase: event.phase,
                    pointer_id: event.pointer_id,
                    x: event.x,
                    y: event.y,
                    coordinate_width: event.coordinate_width,
                    coordinate_height: event.coordinate_height,
                    button: event.button,
                    buttons: event.buttons,
                    wheel_horizontal: event.wheel_horizontal,
                    wheel_vertical: event.wheel_vertical,
                    timestamp: event.timestamp,
                },
            )
            .is_ok()
    }
}

#[derive(Default)]
pub struct SurfaceCapture {
    events: Option<SurfaceEventReceiver>,
    frames: HashMap<u64, SurfaceFrameReceiver>,
    connections: HashMap<TrackedSurfaceId, u64>,
    pointer: SurfacePointerSender,
}

pub enum SurfaceCaptureEvent {
    Offered(SurfaceOffer),
    Frame {
        connection: u64,
        update: SurfaceFrameUpdate,
    },
    Removed {
        id: TrackedSurfaceId,
        connection: u64,
    },
}

pub struct AcceptedSurface {
    pub surface: TrackedSurfaceHandle,
    pub connection: u64,
    pub replaced_connection: Option<u64>,
}

impl SurfaceCapture {
    pub fn attach(&mut self, instance: &EmulatorInstance) {
        self.detach();
        self.events = Some(instance.subscribe_surfaces());
    }

    pub fn detach(&mut self) {
        self.events = None;
        self.frames.clear();
        self.connections.clear();
        self.pointer.targets.clear();
    }

    pub fn try_recv(&mut self) -> Result<SurfaceCaptureEvent, crossfire::TryRecvError> {
        if let Some(event) = self
            .events
            .as_ref()
            .and_then(|events| events.try_recv().ok())
        {
            return match event {
                SurfaceEvent::Offered(offer) => Ok(SurfaceCaptureEvent::Offered(offer)),
                SurfaceEvent::Removed { id, connection } => {
                    self.remove_connection(id, connection);
                    Ok(SurfaceCaptureEvent::Removed { id, connection })
                }
            };
        }

        for (&connection, frames) in &mut self.frames {
            if let Ok(Some(update)) = frames.try_next_frame() {
                if let Some(mut target) = self.pointer.targets.get_mut(&connection) {
                    target.remember(update);
                }
                return Ok(SurfaceCaptureEvent::Frame { connection, update });
            }
        }
        Err(crossfire::TryRecvError::Empty)
    }

    pub fn accept(&mut self, offer: SurfaceOffer) -> Result<AcceptedSurface, SurfaceError> {
        let connection = offer.connection_id();
        let replaced_connection = offer.replaced_connection_id();
        let id = offer.id();
        let protocol_surface_id = offer.stream().surface_id.0;
        let surface = offer.accept()?;
        self.frames.insert(connection, surface.subscribe_frames());
        self.pointer.targets.insert(
            connection,
            PointerTarget {
                protocol_surface_id,
                surface: surface.clone(),
                frames: VecDeque::new(),
            },
        );
        if let Some(previous) = self.connections.insert(id, connection)
            && previous != connection
        {
            self.frames.remove(&previous);
            self.pointer.targets.remove(&previous);
        }
        if let Some(previous) = replaced_connection {
            self.frames.remove(&previous);
            self.pointer.targets.remove(&previous);
        }
        Ok(AcceptedSurface {
            surface,
            connection,
            replaced_connection,
        })
    }

    pub fn reject(&self, offer: SurfaceOffer) {
        offer.reject();
    }

    pub fn revoke(&mut self, connection: u64) {
        self.frames.remove(&connection);
        self.pointer.targets.remove(&connection);
        self.connections.retain(|_, active| *active != connection);
    }

    pub fn pointer_sender(&self) -> SurfacePointerSender {
        self.pointer.clone()
    }

    fn remove_connection(&mut self, id: TrackedSurfaceId, connection: u64) {
        self.frames.remove(&connection);
        self.pointer.targets.remove(&connection);
        if self.connections.get(&id) == Some(&connection) {
            self.connections.remove(&id);
        }
    }
}
