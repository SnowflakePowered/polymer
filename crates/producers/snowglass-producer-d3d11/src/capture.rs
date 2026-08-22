//! Swapchain capture state and viewer handoff.

use crate::{D3d11RingMode, D3d11SwapchainCapture, Tracking};
use parking_lot::Mutex;
use snowglass_hook_dxgi::{SwapchainOwner, SwapchainOwnerHandle};
use snowglass_ipc::{LocalSocket, Message, StreamReady};
use snowglass_platform_windows::{ProcessId, duplicate_handle_into_process, performance_counter};
use snowglass_protocol::{
    AdapterLuid, ConsumerApi, ExternalImageKind, ExternalSyncKind, FrameReady, MAX_IMAGE_SLOTS,
    PROTOCOL_VERSION, PresentationTransform, StreamId, SurfaceId, transport_unorm_dxgi_format,
};
use std::{ffi::c_void, sync::Arc};
use windows::{
    Win32::Graphics::Direct3D11::ID3D11Device,
    core::{IUnknown, Interface},
};

struct CaptureState {
    capture: D3d11SwapchainCapture,
    ipc: Option<LocalSocket>,
    frame_number: u64,
    copy_failures: u64,
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
/// Device tracking is fixed at claim time; the ring behind the mutex is created
/// on demand and released across resizes.
pub struct SwapchainCapture {
    stream_id: u64,
    tracking: Arc<Tracking>,
    capture: Mutex<Capture>,
}

/// Claims a DXGI swapchain created for a D3D11 device, returning the
/// per-swapchain owner the central hook drives from its trampolines.
///
/// # Safety
///
/// `swapchain` and `creation_device` must be null or point to live COM
/// interfaces.
pub unsafe fn claim(
    swapchain: *mut c_void,
    creation_device: *mut c_void,
    tracking: Arc<Tracking>,
) -> Option<SwapchainOwnerHandle> {
    if swapchain.is_null() || creation_device.is_null() {
        return None;
    }
    let is_d3d11 = unsafe { IUnknown::from_raw_borrowed(&creation_device) }
        .is_some_and(|unknown| unknown.cast::<ID3D11Device>().is_ok());
    if !is_d3d11 {
        return None;
    }
    Some(SwapchainOwnerHandle::new(SwapchainCapture {
        stream_id: swapchain as u64,
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
        let presentation = self.tracking.take_swapchain_presentation(swapchain);
        let tracked_source = presentation
            .as_ref()
            .and_then(|value| value.source.as_ref());
        let tracked_region = tracked_source
            .is_none()
            .then(|| presentation.as_ref().map(|value| value.region))
            .flatten();
        let mut capture = self.capture.lock();
        if let Some(source) = tracked_source
            && matches!(&*capture, Capture::Active(state) if {
                let desc = state.capture.ring().desc();
                (desc.width, desc.height, desc.format)
                    != (
                        source.width,
                        source.height,
                        transport_unorm_dxgi_format(source.format),
                    )
            })
        {
            *capture = Capture::Idle;
        }
        if matches!(*capture, Capture::Idle) {
            let connection = match begin_viewer_connection() {
                Ok(connection) => Some(connection),
                Err(error) => {
                    tracing::debug!("snowglass: D3D11 viewer connection failed: {error}");
                    None
                }
            };

            let mode_chain: &[D3d11RingMode] = if connection
                .as_ref()
                .is_some_and(|value| value.preferred_consumer == ConsumerApi::D3d12)
            {
                if snowglass_hook::config::force_keyed_mutex() {
                    &[D3d11RingMode::KeyedMutexNoFence, D3d11RingMode::KeyedMutex]
                } else {
                    &[
                        D3d11RingMode::Fence,
                        D3d11RingMode::KeyedMutexNoFence,
                        D3d11RingMode::KeyedMutex,
                    ]
                }
            } else {
                &[D3d11RingMode::KeyedMutex]
            };
            let mut ring = None;
            for (index, &mode) in mode_chain.iter().enumerate() {
                match unsafe { D3d11SwapchainCapture::new(swapchain, 3, mode, tracked_source) } {
                    Ok(value) => {
                        ring = Some(value);
                        break;
                    }
                    Err(error) if index + 1 < mode_chain.len() => {
                        tracing::debug!(
                            "snowglass: D3D11 {mode:?} capture unavailable, trying {:?}: {error}",
                            mode_chain[index + 1]
                        );
                    }
                    Err(error) => {
                        tracing::debug!("snowglass: D3D11 capture creation failed: {error}");
                    }
                }
            }
            let Some(ring) = ring else {
                return;
            };
            let ipc = connection.and_then(|mut connection| {
                match send_stream_ready(&mut connection, &ring) {
                    Ok(()) => Some(connection.socket),
                    Err(error) => {
                        tracing::debug!("snowglass: D3D11 viewer connection failed: {error}");
                        None
                    }
                }
            });
            let desc = ring.ring().desc();
            tracing::debug!(
                "snowglass: attached D3D11 {}x{} {}-slot shared texture ring",
                desc.width,
                desc.height,
                desc.slots
            );
            *capture = Capture::Active(Box::new(CaptureState {
                capture: ring,
                ipc,
                frame_number: 0,
                copy_failures: 0,
            }));
        }
        let Capture::Active(state) = &mut *capture else {
            return;
        };
        let submission = match unsafe { state.capture.copy_frame(tracked_region, tracked_source) } {
            Ok(submission) => submission,
            Err(error) => {
                state.copy_failures += 1;
                if state.copy_failures == 1 || state.copy_failures % 120 == 0 {
                    tracing::debug!(
                        "snowglass: D3D11 capture copy deferred ({error}, count={})",
                        state.copy_failures
                    );
                }
                return;
            }
        };
        state.frame_number = state.frame_number.wrapping_add(1);
        if state.frame_number == 1 || state.frame_number % 120 == 0 {
            tracing::debug!(
                "snowglass: submitted D3D11 capture {}x{} (count={})",
                submission.width,
                submission.height,
                state.frame_number
            );
        }
        let capture_desc = state.capture.ring().desc();
        let presentation_extent = presentation
            .as_ref()
            .map_or((capture_desc.width, capture_desc.height), |presentation| {
                presentation.target_extent
            });
        let presentation_rect = presentation.as_ref().map_or(
            (
                submission.capture_x,
                submission.capture_y,
                submission.width,
                submission.height,
            ),
            |presentation| {
                (
                    presentation.region.x,
                    presentation.region.y,
                    presentation.region.width,
                    presentation.region.height,
                )
            },
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
            frame_number: state.frame_number,
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
            tracing::debug!("snowglass: viewer released D3D11 stream; capture parked until resize",);
            *capture = Capture::Declined;
        }
    }

    /// Releases the capture ring before the swapchain buffers resize.
    ///
    /// # Safety
    ///
    /// `swapchain` must point to the live swapchain being resized.
    unsafe fn before_resize(&self, _swapchain: *mut c_void) {
        let previous = std::mem::replace(&mut *self.capture.lock(), Capture::Idle);
        if matches!(previous, Capture::Active(_)) {
            tracing::debug!("snowglass: released D3D11 capture for ResizeBuffers");
        }
    }
}

struct ViewerConnection {
    socket: LocalSocket,
    viewer_pid: u32,
    preferred_consumer: ConsumerApi,
}

fn begin_viewer_connection() -> std::io::Result<ViewerConnection> {
    let session = snowglass_hook::config::session()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::NotFound, error))?;
    let mut socket = LocalSocket::connect(&session)?;
    match socket.receive()? {
        Message::Hello(hello) if hello.protocol_version == PROTOCOL_VERSION => {
            socket.enable_frame_channel(hello.process_id)?;
            Ok(ViewerConnection {
                socket,
                viewer_pid: hello.process_id,
                preferred_consumer: hello.preferred_consumer,
            })
        }
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "viewer did not begin with a compatible Hello message",
        )),
    }
}

fn send_stream_ready(
    connection: &mut ViewerConnection,
    capture: &D3d11SwapchainCapture,
) -> std::io::Result<()> {
    let viewer_pid = connection.viewer_pid;
    let ring = capture.ring();
    if ring.slots().len() > MAX_IMAGE_SLOTS {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "D3D11 image ring exceeds the protocol capacity",
        ));
    }
    let mut image_handles = [0i64; MAX_IMAGE_SLOTS];
    for (destination, slot) in image_handles.iter_mut().zip(ring.slots()) {
        *destination =
            duplicate_handle_into_process(slot.raw_handle() as isize, ProcessId(viewer_pid))?
                as i64;
    }
    let fence = match ring.raw_fence_handle() {
        Some(raw) => duplicate_handle_into_process(raw as isize, ProcessId(viewer_pid))? as i64,
        None => 0,
    };
    let (image_kind, sync_kind, adapter_luid, consumer_fence) = match ring.mode() {
        D3d11RingMode::Fence => {
            let raw_consumer_fence = ring.raw_consumer_fence_handle().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "fence-mode D3D11 ring is missing its consumer fence",
                )
            })?;
            (
                ExternalImageKind::D3d11D3d12Resource,
                // The shared D3D11 fences open as ID3D12Fence on the consumer.
                ExternalSyncKind::D3d12Fence,
                capture.adapter_luid(),
                duplicate_handle_into_process(raw_consumer_fence as isize, ProcessId(viewer_pid))?
                    as i64,
            )
        }
        // The D3D12 consumer acquires the keyed mutexes through D3D11On12.
        D3d11RingMode::KeyedMutexNoFence => (
            ExternalImageKind::D3d11SharedResourceNt,
            ExternalSyncKind::D3d11KeyedMutex,
            capture.adapter_luid(),
            0,
        ),
        D3d11RingMode::KeyedMutex => (
            ExternalImageKind::D3d11SharedResourceNt,
            ExternalSyncKind::D3d11Fence,
            AdapterLuid {
                low_part: 0,
                high_part: 0,
            },
            0,
        ),
    };
    let desc = ring.desc();
    connection.socket.send(Message::StreamReady(StreamReady {
        producer_process_id: std::process::id(),
        surface_id: SurfaceId(capture.source_window() as u64),
        adapter_luid,
        source_window: capture.source_window() as i64,
        width: desc.width,
        height: desc.height,
        format: desc.format,
        image_kind,
        sync_kind,
        slot_count: desc.slots,
        image_handles,
        timeline_semaphore_handle: fence,
        consumer_semaphore_handle: consumer_fence,
        bottom_up: false,
    }))?;
    tracing::debug!("snowglass: connected D3D11 ring to viewer process {viewer_pid}");
    Ok(())
}
