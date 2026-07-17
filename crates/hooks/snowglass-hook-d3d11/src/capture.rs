//! Swapchain capture state and viewer handoff.

use snowglass_ipc::{LocalSocket, Message, StreamReady};
use snowglass_platform_windows::{ProcessId, duplicate_handle_into_process, performance_counter};
use snowglass_producer_d3d11::D3d11SwapchainCapture;
use snowglass_protocol::{
    ExternalImageKind, ExternalSyncKind, FrameReady, MAX_IMAGE_SLOTS, PROTOCOL_VERSION, StreamId,
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

static CAPTURES: OnceLock<Mutex<HashMap<usize, CaptureState>>> = OnceLock::new();

fn captures() -> &'static Mutex<HashMap<usize, CaptureState>> {
    CAPTURES.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) unsafe fn before_present(swapchain: *mut c_void) {
    if snowglass_platform_windows::vulkan_capture_active() {
        return;
    }
    let mut captures = captures().lock().unwrap_or_else(|error| error.into_inner());
    if let std::collections::hash_map::Entry::Vacant(entry) = captures.entry(swapchain as usize) {
        let capture = match unsafe { D3d11SwapchainCapture::new(swapchain, 3) } {
            Ok(capture) => capture,
            Err(error) => {
                crate::d3d11_log(format!("snowglass: D3D11 capture creation failed: {error}"));
                return;
            }
        };
        let ipc = match connect_viewer(&capture) {
            Ok(ipc) => Some(ipc),
            Err(error) => {
                crate::d3d11_log(format!(
                    "snowglass: D3D11 viewer connection failed: {error}"
                ));
                None
            }
        };
        let desc = capture.ring().desc();
        crate::d3d11_log(format!(
            "snowglass: attached D3D11 {}x{} {}-slot shared texture ring",
            desc.width, desc.height, desc.slots
        ));
        entry.insert(CaptureState {
            capture,
            ipc,
            frame_number: 0,
            copy_failures: 0,
        });
    }
    if let Some(state) = captures.get_mut(&(swapchain as usize)) {
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
            state.ipc = None;
        }
    }
}

pub(crate) unsafe fn before_resize(swapchain: *mut c_void) {
    if captures()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&(swapchain as usize))
        .is_some()
    {
        crate::d3d11_log("snowglass: released D3D11 capture for ResizeBuffers");
    }
}

pub(crate) unsafe fn destroyed(swapchain: *mut c_void) {
    unsafe { before_resize(swapchain) };
}

fn connect_viewer(capture: &D3d11SwapchainCapture) -> std::io::Result<LocalSocket> {
    let session = std::env::var("SNOWGLASS_SESSION")
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::NotFound, error))?;
    let mut socket = LocalSocket::connect(&session)?;
    let viewer_pid = match socket.receive()? {
        Message::Hello(hello) if hello.protocol_version == PROTOCOL_VERSION => hello.process_id,
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "viewer did not begin with a compatible Hello message",
            ));
        }
    };
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
    let fence =
        duplicate_handle_into_process(ring.raw_fence_handle() as isize, ProcessId(viewer_pid))?
            as i64;
    let desc = ring.desc();
    socket.send(Message::StreamReady(StreamReady {
        producer_process_id: std::process::id(),
        source_window: capture.source_window() as i64,
        width: desc.width,
        height: desc.height,
        format: desc.format,
        image_kind: ExternalImageKind::D3d11SharedResourceNt,
        sync_kind: ExternalSyncKind::D3d11Fence,
        slot_count: desc.slots,
        image_handles,
        timeline_semaphore_handle: fence,
    }))?;
    crate::d3d11_log(format!(
        "snowglass: connected D3D11 ring to viewer process {viewer_pid}"
    ));
    Ok(socket)
}
