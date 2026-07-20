//! Swapchain capture state, copy submission, and viewer handoff.

use crate::{D3d12CaptureSourceState, D3d12SwapchainCapture, Tracking};
use snowglass_hook_dxgi::{SwapchainOwner, SwapchainOwnerHandle};
use snowglass_ipc::{LocalSocket, Message, StreamReady};
use snowglass_platform_windows::{ProcessId, duplicate_handle_into_process, performance_counter};
use snowglass_protocol::{
    AdapterLuid, ExternalImageKind, ExternalSyncKind, FrameReady, MAX_IMAGE_SLOTS,
    PROTOCOL_VERSION, PresentationTransform, StreamId, SurfaceId,
};
use std::{
    ffi::c_void,
    sync::{Arc, Mutex},
};
use windows::{
    Win32::Graphics::Direct3D12::ID3D12CommandQueue,
    core::{IUnknown, Interface},
};

struct CaptureState {
    capture: D3d12SwapchainCapture,
    ipc: Option<LocalSocket>,
    frame: u64,
    copy_errors: u64,
}

/// The capture ring for one swapchain. A ring is created lazily on the first
/// present and dropped when the swapchain resizes; a swapchain the viewer
/// declined stays parked until it resizes or is destroyed, so a rejected
/// surface does not reconnect on every present.
enum Capture {
    Idle,
    Active(Box<CaptureState>),
    Declined,
}

/// Per-swapchain capture state stored in the swapchain's hook context.
///
/// The queue and device tracking are fixed at claim time; the ring behind the
/// mutex is created on demand and released across resizes.
pub struct SwapchainCapture {
    stream_id: u64,
    queue: ID3D12CommandQueue,
    tracking: Arc<Tracking>,
    capture: Mutex<Capture>,
}

/// Claims a DXGI swapchain created for a D3D12 command queue, returning the
/// per-swapchain owner the central hook drives from its trampolines.
///
/// # Safety
///
/// `swapchain` and `creation_device` must point to live COM interfaces.
pub unsafe fn claim(
    swapchain: *mut c_void,
    creation_device: *mut c_void,
    tracking: Arc<Tracking>,
) -> Option<SwapchainOwnerHandle> {
    if creation_device.is_null() {
        return None;
    }
    let stream_id = unsafe { swapchain_key(swapchain) }? as u64;
    let queue = unsafe { IUnknown::from_raw_borrowed(&creation_device) }?
        .cast::<ID3D12CommandQueue>()
        .ok()?;
    Some(SwapchainOwnerHandle::new(SwapchainCapture {
        stream_id,
        queue,
        tracking,
        capture: Mutex::new(Capture::Idle),
    }))
}

impl SwapchainOwner for SwapchainCapture {
    /// Captures the image that is about to be presented.
    ///
    /// # Safety
    ///
    /// `swapchain` must point to the live swapchain being presented.
    unsafe fn before_present(&self, swapchain: *mut c_void) {
        let presentation = unsafe { self.tracking.swapchain_presentation(swapchain) };
        let tracked_source = presentation
            .as_ref()
            .and_then(|value| value.source.as_ref())
            .filter(|source| {
                source
                    .state
                    .is_some_and(|state| matches!(state, D3d12CaptureSourceState::Legacy(_)))
            });
        // A tracked source is copied in its own coordinate system. The draw
        // region belongs to the destination backbuffer and is carried only as
        // presentation metadata for pointer mapping.
        let copy_region = tracked_source
            .is_none()
            .then(|| presentation.as_ref().and_then(|value| value.region))
            .flatten();
        let mut capture = self.capture.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(source) = tracked_source
            && matches!(&*capture, Capture::Active(state) if {
                let desc = state.capture.desc();
                (desc.width, desc.height, desc.format)
                    != (source.width, source.height, source.format)
            })
        {
            *capture = Capture::Idle;
        }
        if matches!(&*capture, Capture::Idle) {
            crate::d3d12_log("snowglass: creating D3D12 shared capture ring");
            let ring = match unsafe {
                D3d12SwapchainCapture::new(
                    swapchain,
                    self.queue.as_raw(),
                    3,
                    tracked_source.map(|source| &source.resource),
                    self.tracking.enhanced_barriers_supported(),
                )
            } {
                Ok(value) => value,
                Err(error) => {
                    crate::d3d12_log(format!("snowglass: D3D12 capture creation failed: {error}"));
                    return;
                }
            };
            crate::d3d12_log("snowglass: connecting D3D12 capture ring to viewer");
            let ipc = connect_viewer(&ring).ok();
            let desc = ring.desc();
            crate::d3d12_log(format!(
                "snowglass: attached D3D12 {}x{} {}-slot shared texture ring",
                desc.width, desc.height, desc.slots
            ));
            *capture = Capture::Active(Box::new(CaptureState {
                capture: ring,
                ipc,
                frame: 0,
                copy_errors: 0,
            }));
        }
        let Capture::Active(state) = &mut *capture else {
            return;
        };
        let submission = match unsafe {
            state.capture.copy_frame(
                copy_region,
                tracked_source.map(|source| {
                    (
                        &source.resource,
                        source.state.expect("tracked source state was filtered"),
                    )
                }),
            )
        } {
            Ok(Some(submission)) => submission,
            Ok(None) => return,
            Err(error) => {
                state.copy_errors = state.copy_errors.wrapping_add(1);
                if state.copy_errors == 1 || state.copy_errors % 120 == 0 {
                    crate::d3d12_log(format!(
                        "snowglass: D3D12 capture copy failed: {error} (count={})",
                        state.copy_errors
                    ));
                }
                return;
            }
        };
        state.frame = state.frame.wrapping_add(1);
        if state.frame == 1 || state.frame % 120 == 0 {
            crate::d3d12_log(format!(
                "snowglass: submitted D3D12 capture {}x{} at {},{} (count={})",
                submission.width,
                submission.height,
                submission.capture_x,
                submission.capture_y,
                state.frame
            ));
        }
        let capture_desc = state.capture.desc();
        let presentation_extent = presentation
            .as_ref()
            .map_or((capture_desc.width, capture_desc.height), |value| {
                value.target_extent
            });
        let presentation_rect = presentation.as_ref().and_then(|value| value.region).map_or(
            (
                submission.capture_x,
                submission.capture_y,
                submission.width,
                submission.height,
            ),
            |region| (region.x, region.y, region.width, region.height),
        );
        let message = Message::FrameReady(FrameReady {
            stream_id: StreamId(self.stream_id),
            slot: submission.slot,
            render_width: submission.width,
            render_height: submission.height,
            capture_x: presentation_rect.0,
            capture_y: presentation_rect.1,
            capture_width: presentation_rect.2,
            capture_height: presentation_rect.3,
            presentation_width: presentation_extent.0,
            presentation_height: presentation_extent.1,
            presentation_transform: PresentationTransform::axis_aligned(
                presentation_rect.0,
                presentation_rect.1,
                presentation_rect.2,
                presentation_rect.3,
            ),
            frame_number: state.frame,
            sync_value: submission.sync_value,
            timestamp_qpc: performance_counter().unwrap_or(0),
        });
        if state
            .ipc
            .as_mut()
            .is_some_and(|ipc| ipc.send(message).is_err())
        {
            // The viewer declined or preempted this stream; release the ring
            // instead of copying frames nobody consumes.
            crate::d3d12_log(
                "snowglass: viewer released D3D12 stream; capture parked until resize",
            );
            *capture = Capture::Declined;
        }
    }

    /// Releases the capture ring and tracking references before the swapchain
    /// buffers resize.
    ///
    /// # Safety
    ///
    /// `swapchain` must point to the live swapchain being resized.
    unsafe fn before_resize(&self, swapchain: *mut c_void) {
        let previous = std::mem::replace(
            &mut *self.capture.lock().unwrap_or_else(|e| e.into_inner()),
            Capture::Idle,
        );
        crate::d3d12_log(format!(
            "snowglass: D3D12 resize released capture state: {}",
            matches!(previous, Capture::Active(_))
        ));
        unsafe { self.tracking.before_swapchain_resize(swapchain) };
    }
}

unsafe fn swapchain_key(swapchain: *mut c_void) -> Option<usize> {
    let unknown = unsafe { IUnknown::from_raw_borrowed(&swapchain) }?;
    unknown
        .cast::<IUnknown>()
        .ok()
        .map(|identity| identity.as_raw() as usize)
}

fn connect_viewer(capture: &D3d12SwapchainCapture) -> std::io::Result<LocalSocket> {
    let session = snowglass_hook::config::session()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::NotFound, e))?;
    let mut socket = LocalSocket::connect(&session)?;
    let pid = match socket.receive()? {
        Message::Hello(h) if h.protocol_version == PROTOCOL_VERSION => h.process_id,
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "incompatible viewer",
            ));
        }
    };
    let mut image_handles = [0i64; MAX_IMAGE_SLOTS];
    for (dst, slot) in image_handles.iter_mut().zip(capture.slots()) {
        *dst = duplicate_handle_into_process(slot.raw_handle(), ProcessId(pid))? as i64;
    }
    let desc = capture.desc();
    let (adapter_luid_low, adapter_luid_high) = capture.adapter_luid();
    socket.send(Message::StreamReady(StreamReady {
        producer_process_id: std::process::id(),
        surface_id: SurfaceId(capture.source_window() as u64),
        adapter_luid: AdapterLuid {
            low_part: adapter_luid_low,
            high_part: adapter_luid_high,
        },
        source_window: capture.source_window() as i64,
        width: desc.width,
        height: desc.height,
        format: desc.format,
        image_kind: ExternalImageKind::D3d12Resource,
        sync_kind: ExternalSyncKind::D3d12Fence,
        slot_count: desc.slots,
        image_handles,
        timeline_semaphore_handle: duplicate_handle_into_process(
            capture.raw_fence_handle(),
            ProcessId(pid),
        )? as i64,
        consumer_semaphore_handle: duplicate_handle_into_process(
            capture.raw_consumer_fence_handle(),
            ProcessId(pid),
        )? as i64,
        bottom_up: false,
    }))?;
    Ok(socket)
}
