use polymer_ipc::StreamReady;
use polymer_protocol::{
    AdapterLuid, ExternalImageKind, ExternalSyncKind, MAX_IMAGE_SLOTS, SurfaceId,
};
use std::io;

/// Backend export metadata. Handles remain owned by the native resources;
/// publication duplicates them without transferring the producer's ownership.
pub struct ExportDescriptor<'a> {
    #[cfg(target_os = "macos")]
    pub resources: polymer_ipc::xpc::MetalResources,
    pub surface_id: SurfaceId,
    pub adapter_luid: AdapterLuid,
    pub source_window: i64,
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub image_kind: ExternalImageKind,
    pub sync_kind: ExternalSyncKind,
    pub image_handles: &'a [i64],
    pub timeline_semaphore_handle: i64,
    pub consumer_semaphore_handle: i64,
    pub bottom_up: bool,
}

impl ExportDescriptor<'_> {
    pub(super) fn into_ready(self) -> io::Result<StreamReady> {
        if self.image_handles.is_empty() || self.image_handles.len() > MAX_IMAGE_SLOTS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid capture slot count",
            ));
        }
        let mut image_handles = [0; MAX_IMAGE_SLOTS];
        image_handles[..self.image_handles.len()].copy_from_slice(self.image_handles);
        Ok(StreamReady {
            producer_process_id: std::process::id(),
            surface_id: self.surface_id,
            adapter_luid: self.adapter_luid,
            source_window: self.source_window,
            width: self.width,
            height: self.height,
            format: self.format,
            image_kind: self.image_kind,
            sync_kind: self.sync_kind,
            slot_count: self.image_handles.len() as u32,
            image_handles,
            timeline_semaphore_handle: self.timeline_semaphore_handle,
            consumer_semaphore_handle: self.consumer_semaphore_handle,
            bottom_up: self.bottom_up,
        })
    }
}
