//! Swapchain capture state, copy submission, and viewer handoff.

use crate::{D3d12CaptureSourceState, D3d12SwapchainCapture};
use snowglass_ipc::{LocalSocket, Message, StreamReady};
use snowglass_platform_windows::{ProcessId, duplicate_handle_into_process, performance_counter};
use snowglass_protocol::{
    AdapterLuid, ExternalImageKind, ExternalSyncKind, FrameReady, MAX_IMAGE_SLOTS,
    PROTOCOL_VERSION, PresentationMappingKind, StreamId, SurfaceId,
};
use std::{
    collections::HashMap,
    ffi::c_void,
    sync::{Mutex, OnceLock},
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

/// A swapchain the viewer declined stays parked until it resizes or is
/// destroyed, so a rejected surface does not reconnect on every present.
enum CaptureEntry {
    Active(Box<CaptureState>),
    Declined,
}
static QUEUES: OnceLock<Mutex<HashMap<usize, ID3D12CommandQueue>>> = OnceLock::new();
static CAPTURES: OnceLock<Mutex<HashMap<usize, CaptureEntry>>> = OnceLock::new();
static ALIASES: OnceLock<Mutex<HashMap<usize, usize>>> = OnceLock::new();
fn queues() -> &'static Mutex<HashMap<usize, ID3D12CommandQueue>> {
    QUEUES.get_or_init(Default::default)
}
fn captures() -> &'static Mutex<HashMap<usize, CaptureEntry>> {
    CAPTURES.get_or_init(Default::default)
}
fn aliases() -> &'static Mutex<HashMap<usize, usize>> {
    ALIASES.get_or_init(Default::default)
}

/// Claims a DXGI swapchain created for a D3D12 command queue.
///
/// # Safety
///
/// `swapchain` and `creation_device` must point to live COM interfaces.
pub unsafe fn claim(swapchain: *mut c_void, creation_device: *mut c_void) -> bool {
    if creation_device.is_null() {
        return false;
    }
    let (key, queue) = unsafe {
        let Some(key) = swapchain_key(swapchain) else {
            return false;
        };
        let Some(unknown) = IUnknown::from_raw_borrowed(&creation_device) else {
            return false;
        };
        let Ok(queue) = unknown.cast::<ID3D12CommandQueue>() else {
            return false;
        };
        (key, queue)
    };
    queues()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, queue);
    aliases()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(swapchain as usize, key);
    true
}

/// Captures the image that is about to be presented.
///
/// # Safety
///
/// `swapchain` must point to a live DXGI swapchain previously passed to [`claim`].
pub unsafe fn before_present(swapchain: *mut c_void) {
    let Some(key) = (unsafe { swapchain_key(swapchain) }) else {
        return;
    };
    aliases()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(swapchain as usize, key);
    let queue = queues()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
        .cloned();
    let Some(queue) = queue else {
        return;
    };
    let presentation = unsafe { crate::swapchain_presentation(swapchain) };
    let tracked_region = presentation.as_ref().and_then(|value| value.region);
    let tracked_source = presentation
        .as_ref()
        .and_then(|value| value.source.as_ref())
        .filter(|source| {
            source
                .state
                .is_some_and(|state| matches!(state, D3d12CaptureSourceState::Legacy(_)))
        });
    let mut states = captures().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(source) = tracked_source
        && states.get(&key).is_some_and(|entry| match entry {
            CaptureEntry::Active(state) => {
                let desc = state.capture.desc();
                (desc.width, desc.height, desc.format)
                    != (source.width, source.height, source.format)
            }
            CaptureEntry::Declined => false,
        })
    {
        states.remove(&key);
    }
    if let std::collections::hash_map::Entry::Vacant(entry) = states.entry(key) {
        crate::d3d12_log("snowglass: creating D3D12 shared capture ring");
        let capture = match unsafe {
            D3d12SwapchainCapture::new(
                swapchain,
                queue.as_raw(),
                3,
                tracked_source.map(|source| &source.resource),
                crate::enhanced_barriers_supported(),
            )
        } {
            Ok(value) => value,
            Err(error) => {
                crate::d3d12_log(format!("snowglass: D3D12 capture creation failed: {error}"));
                return;
            }
        };
        crate::d3d12_log("snowglass: connecting D3D12 capture ring to viewer");
        let ipc = connect_viewer(&capture).ok();
        let desc = capture.desc();
        crate::d3d12_log(format!(
            "snowglass: attached D3D12 {}x{} {}-slot shared texture ring",
            desc.width, desc.height, desc.slots
        ));
        entry.insert(CaptureEntry::Active(Box::new(CaptureState {
            capture,
            ipc,
            frame: 0,
            copy_errors: 0,
        })));
    }
    let Some(CaptureEntry::Active(state)) = states.get_mut(&key) else {
        return;
    };
    let submission = match unsafe {
        state.capture.copy_frame(
            tracked_region,
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
    let presentation_rect = if tracked_source.is_some() {
        (0, 0, presentation_extent.0, presentation_extent.1)
    } else {
        (
            submission.capture_x,
            submission.capture_y,
            submission.width,
            submission.height,
        )
    };
    let message = Message::FrameReady(FrameReady {
        stream_id: StreamId(key as u64),
        slot: submission.slot,
        render_width: submission.width,
        render_height: submission.height,
        capture_x: presentation_rect.0,
        capture_y: presentation_rect.1,
        capture_width: presentation_rect.2,
        capture_height: presentation_rect.3,
        presentation_width: presentation_extent.0,
        presentation_height: presentation_extent.1,
        mapping_kind: PresentationMappingKind::AxisAligned,
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
        crate::d3d12_log("snowglass: viewer released D3D12 stream; capture parked until resize");
        states.insert(key, CaptureEntry::Declined);
    }
}

/// Releases capture and tracking references before the swapchain buffers resize.
///
/// # Safety
///
/// `swapchain` must point to a live DXGI swapchain previously passed to [`claim`].
pub unsafe fn before_resize(swapchain: *mut c_void) {
    let Some(key) = (unsafe { swapchain_key(swapchain) }) else {
        return;
    };
    let removed = captures()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&key);
    crate::d3d12_log(format!(
        "snowglass: D3D12 resize released capture state: {}",
        matches!(removed, Some(CaptureEntry::Active(_)))
    ));
    unsafe { crate::before_swapchain_resize(swapchain) };
}

/// Releases all producer state associated with a destroyed swapchain.
///
/// # Safety
///
/// `swapchain` must be the pointer previously passed to [`claim`].
pub unsafe fn destroyed(swapchain: *mut c_void) {
    let key = aliases()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(swapchain as usize))
        .unwrap_or(swapchain as usize);
    captures()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&key);
    queues()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&key);
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
