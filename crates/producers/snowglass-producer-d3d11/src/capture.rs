//! Swapchain capture state and viewer handoff.

use crate::{D3d11RingMode, D3d11SwapchainCapture};
use snowglass_ipc::{LocalSocket, Message, StreamReady};
use snowglass_platform_windows::{ProcessId, duplicate_handle_into_process, performance_counter};
use snowglass_protocol::{
    AdapterLuid, ConsumerApi, ExternalImageKind, ExternalSyncKind, FrameReady, MAX_IMAGE_SLOTS,
    PROTOCOL_VERSION, StreamId,
};
use std::{
    collections::HashMap,
    ffi::c_void,
    sync::{Mutex, OnceLock},
};

struct CaptureState {
    capture: D3d11SwapchainCapture,
    ipc: Option<LocalSocket>,
    frame_number: u64,
    copy_failures: u64,
}

/// A swapchain the viewer declined stays parked until it resizes or is
/// destroyed, so a rejected surface does not reconnect on every present.
enum CaptureEntry {
    Active(Box<CaptureState>),
    Declined,
}

static CAPTURES: OnceLock<Mutex<HashMap<usize, CaptureEntry>>> = OnceLock::new();

fn captures() -> &'static Mutex<HashMap<usize, CaptureEntry>> {
    CAPTURES.get_or_init(|| Mutex::new(HashMap::new()))
}

pub unsafe fn before_present(swapchain: *mut c_void) {
    let mut captures = captures().lock().unwrap_or_else(|error| error.into_inner());
    if let std::collections::hash_map::Entry::Vacant(entry) = captures.entry(swapchain as usize) {
        let connection = match begin_viewer_connection() {
            Ok(connection) => Some(connection),
            Err(error) => {
                crate::d3d11_log(format!(
                    "snowglass: D3D11 viewer connection failed: {error}"
                ));
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
        let mut capture = None;
        for (index, &mode) in mode_chain.iter().enumerate() {
            match unsafe { D3d11SwapchainCapture::new(swapchain, 3, mode) } {
                Ok(value) => {
                    capture = Some(value);
                    break;
                }
                Err(error) if index + 1 < mode_chain.len() => {
                    crate::d3d11_log(format!(
                        "snowglass: D3D11 {mode:?} capture unavailable, trying {:?}: {error}",
                        mode_chain[index + 1]
                    ));
                }
                Err(error) => {
                    crate::d3d11_log(format!("snowglass: D3D11 capture creation failed: {error}"));
                }
            }
        }
        let Some(capture) = capture else {
            return;
        };
        let ipc = connection.and_then(|mut connection| {
            match send_stream_ready(&mut connection, &capture) {
                Ok(()) => Some(connection.socket),
                Err(error) => {
                    crate::d3d11_log(format!(
                        "snowglass: D3D11 viewer connection failed: {error}"
                    ));
                    None
                }
            }
        });
        let desc = capture.ring().desc();
        crate::d3d11_log(format!(
            "snowglass: attached D3D11 {}x{} {}-slot shared texture ring",
            desc.width, desc.height, desc.slots
        ));
        entry.insert(CaptureEntry::Active(Box::new(CaptureState {
            capture,
            ipc,
            frame_number: 0,
            copy_failures: 0,
        })));
    }
    if let Some(CaptureEntry::Active(state)) = captures.get_mut(&(swapchain as usize)) {
        let tracked_region = crate::tracking::take_swapchain_region(swapchain);
        let submission = match unsafe { state.capture.copy_frame(tracked_region) } {
            Ok(submission) => submission,
            Err(error) => {
                state.copy_failures += 1;
                if state.copy_failures == 1 || state.copy_failures % 120 == 0 {
                    crate::d3d11_log(format!(
                        "snowglass: D3D11 capture copy deferred ({error}, count={})",
                        state.copy_failures
                    ));
                }
                return;
            }
        };
        state.frame_number = state.frame_number.wrapping_add(1);
        if state.frame_number == 1 || state.frame_number % 120 == 0 {
            crate::d3d11_log(format!(
                "snowglass: submitted D3D11 capture {}x{} (count={})",
                submission.width, submission.height, state.frame_number
            ));
        }
        let message = Message::FrameReady(FrameReady {
            stream_id: StreamId(swapchain as usize as u64),
            slot: submission.slot,
            render_width: submission.width,
            render_height: submission.height,
            capture_x: submission.capture_x,
            capture_y: submission.capture_y,
            capture_width: submission.width,
            capture_height: submission.height,
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
            crate::d3d11_log(
                "snowglass: viewer released D3D11 stream; capture parked until resize",
            );
            captures.insert(swapchain as usize, CaptureEntry::Declined);
        }
    }
}

pub unsafe fn before_resize(swapchain: *mut c_void) {
    if matches!(
        captures()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&(swapchain as usize)),
        Some(CaptureEntry::Active(_))
    ) {
        crate::d3d11_log("snowglass: released D3D11 capture for ResizeBuffers");
    }
}

pub unsafe fn destroyed(swapchain: *mut c_void) {
    unsafe { before_resize(swapchain) };
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
    }))?;
    crate::d3d11_log(format!(
        "snowglass: connected D3D11 ring to viewer process {viewer_pid}"
    ));
    Ok(())
}
