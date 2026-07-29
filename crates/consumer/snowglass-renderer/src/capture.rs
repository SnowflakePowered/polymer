//! Renderer-side consumption of runtime-managed emulator surfaces.

use snowglass_runtime::{
    EmulatorInstance, SurfaceError, SurfaceEvent, SurfaceEventReceiver, SurfaceFrameReceiver,
    SurfaceFrameUpdate, SurfaceOffer, TrackedSurfaceHandle, TrackedSurfaceId,
};
use std::collections::HashMap;

#[derive(Default)]
pub struct SurfaceCapture {
    events: Option<SurfaceEventReceiver>,
    frames: HashMap<u64, SurfaceFrameReceiver>,
    connections: HashMap<TrackedSurfaceId, u64>,
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
                return Ok(SurfaceCaptureEvent::Frame { connection, update });
            }
        }
        Err(crossfire::TryRecvError::Empty)
    }

    pub fn accept(&mut self, offer: SurfaceOffer) -> Result<AcceptedSurface, SurfaceError> {
        let connection = offer.connection_id();
        let replaced_connection = offer.replaced_connection_id();
        let id = offer.id();
        let surface = offer.accept()?;
        self.frames.insert(connection, surface.subscribe_frames());
        if let Some(previous) = self.connections.insert(id, connection)
            && previous != connection
        {
            self.frames.remove(&previous);
        }
        if let Some(previous) = replaced_connection {
            self.frames.remove(&previous);
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
        self.connections.retain(|_, active| *active != connection);
    }

    fn remove_connection(&mut self, id: TrackedSurfaceId, connection: u64) {
        self.frames.remove(&connection);
        if self.connections.get(&id) == Some(&connection) {
            self.connections.remove(&id);
        }
    }
}
