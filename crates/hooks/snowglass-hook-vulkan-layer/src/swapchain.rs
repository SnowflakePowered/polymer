//! Swapchain capture, export, and presentation.

use crate::layer::*;
use crate::tracking::RenderTargetCandidate;
use ash::{vk, vk::Handle};
use dashmap::DashMap;
use snowglass_hook::HookChain;
use snowglass_ipc::{LocalSocket, Message, StreamReady};
use snowglass_platform_windows::{ProcessId, duplicate_handle_into_process, performance_counter};
use snowglass_producer_vulkan::{
    ExportImageRing, ExportImageRingDesc, ExportTimelineSemaphore, SwapchainCopyEngine,
};
use snowglass_protocol::{
    AdapterLuid, ExternalImageKind, ExternalSyncKind, FrameReady, MAX_IMAGE_SLOTS,
    PROTOCOL_VERSION, StreamId,
};
use std::sync::{
    LazyLock,
    atomic::{AtomicU64, Ordering},
};

struct SwapchainState {
    device: vk::Device,
    format: vk::Format,
    extent: vk::Extent2D,
    images: Vec<vk::Image>,
    ring: Option<ExportImageRing>,
    timeline: Option<ExportTimelineSemaphore>,
    copy_engine: Option<SwapchainCopyEngine>,
    ipc: Option<LocalSocket>,
    frame_number: u64,
}

static SWAPCHAIN: LazyLock<DashMap<vk::SwapchainKHR, SwapchainState>> =
    LazyLock::new(Default::default);
static SURFACE_WINDOW: LazyLock<DashMap<vk::SurfaceKHR, isize>> = LazyLock::new(Default::default);
static SWAPCHAIN_IMAGE: LazyLock<DashMap<vk::Image, vk::SwapchainKHR>> =
    LazyLock::new(Default::default);
static SWAPCHAINS_CREATED: AtomicU64 = AtomicU64::new(0);
static PRESENTS_SEEN: AtomicU64 = AtomicU64::new(0);
static COPIES_SUBMITTED: AtomicU64 = AtomicU64::new(0);

pub(crate) fn destroy_device(device: vk::Device) {
    {
        SWAPCHAIN.retain(|_, state| state.device != device)
    };
}

pub(crate) fn is_swapchain_image(image: vk::Image) -> bool {
    SWAPCHAIN_IMAGE.contains_key(&image)
}

pub(crate) fn create_win32_surface(
    create_info: *const vk::Win32SurfaceCreateInfoKHR<'_>,
    surface: *mut vk::SurfaceKHR,
    result: vk::Result,
) {
    if result == vk::Result::SUCCESS
        && !create_info.is_null()
        && !surface.is_null()
        && unsafe { *surface } != vk::SurfaceKHR::null()
    {
        let created_surface = unsafe { *surface };
        let source_window = unsafe { (*create_info).hwnd as isize };
        SURFACE_WINDOW.insert(created_surface, source_window);
        layer_log(format!(
            "snowglass: associated Vulkan surface with HWND {:?}",
            unsafe { (*create_info).hwnd }
        ));
    }
}

pub(crate) fn destroy_surface(surface: vk::SurfaceKHR) {
    {
        SURFACE_WINDOW.remove(&surface)
    };
}

pub(crate) fn connect_viewer(
    ring: &ExportImageRing,
    timeline: &ExportTimelineSemaphore,
    extent: vk::Extent2D,
    format: vk::Format,
    source_window: isize,
) -> Option<LocalSocket> {
    let session = std::env::var("SNOWGLASS_SESSION").ok()?;
    let mut socket = match LocalSocket::connect(&session) {
        Ok(socket) => socket,
        Err(error) => {
            layer_log(format!(
                "snowglass: viewer socket connection failed: {error}"
            ));
            return None;
        }
    };
    let viewer_pid = match socket.receive() {
        Ok(Message::Hello(hello)) if hello.protocol_version == PROTOCOL_VERSION => hello.process_id,
        Ok(Message::Hello(hello)) => {
            layer_log(format!(
                "snowglass: viewer protocol {} is incompatible with {}",
                hello.protocol_version, PROTOCOL_VERSION
            ));
            return None;
        }
        Ok(_) => {
            layer_log("snowglass: viewer did not begin with a Hello message");
            return None;
        }
        Err(error) => {
            layer_log(format!("snowglass: viewer handshake failed: {error}"));
            return None;
        }
    };
    if ring.slots().len() > MAX_IMAGE_SLOTS {
        layer_log("snowglass: export ring exceeds protocol slot capacity");
        return None;
    }
    let mut handles = [0; MAX_IMAGE_SLOTS];
    for (destination, slot) in handles.iter_mut().zip(ring.slots()) {
        match duplicate_handle_into_process(slot.raw_handle(), ProcessId(viewer_pid)) {
            Ok(handle) => *destination = handle as i64,
            Err(error) => {
                layer_log(format!(
                    "snowglass: image handle duplication failed: {error}"
                ));
                return None;
            }
        }
    }
    let ready = StreamReady {
        producer_process_id: std::process::id(),
        adapter_luid: AdapterLuid {
            low_part: 0,
            high_part: 0,
        },
        source_window: source_window as i64,
        width: extent.width,
        height: extent.height,
        format: format.as_raw() as u32,
        image_kind: ExternalImageKind::VulkanOpaqueWin32,
        sync_kind: ExternalSyncKind::VulkanTimelineSemaphore,
        slot_count: ring.slots().len() as u32,
        image_handles: handles,
        timeline_semaphore_handle: match duplicate_handle_into_process(
            timeline.raw_handle(),
            ProcessId(viewer_pid),
        ) {
            Ok(handle) => handle as i64,
            Err(error) => {
                layer_log(format!(
                    "snowglass: timeline handle duplication failed: {error}"
                ));
                return None;
            }
        },
        consumer_semaphore_handle: 0,
    };
    if let Err(error) = socket.send(Message::StreamReady(ready)) {
        layer_log(format!("snowglass: StreamReady send failed: {error}"));
        return None;
    }
    layer_log(format!(
        "snowglass: connected export ring to viewer process {viewer_pid}"
    ));
    Some(socket)
}

pub(crate) unsafe fn handle_queue_present(
    queue: vk::Queue,
    present_info: *const vk::PresentInfoKHR<'_>,
    mut next: crate::hooks::QueuePresentContext<'_>,
) -> vk::Result {
    let continuation = *next
        .fp_next()
        .expect("Vulkan around chains always have a terminal");
    let queue_state = { QUEUE.get(&queue).map(|entry| entry.clone()) };
    let result = if let Some(queue_state) = queue_state {
        if !present_info.is_null()
            && unsafe { (*present_info).swapchain_count } == 1
            && !unsafe { (*present_info).p_swapchains }.is_null()
            && !unsafe { (*present_info).p_image_indices }.is_null()
        {
            let original = unsafe { &*present_info };
            let swapchain = unsafe { *original.p_swapchains };
            let image_index = unsafe { *original.p_image_indices };
            let waits = if original.wait_semaphore_count == 0 {
                &[]
            } else {
                unsafe {
                    std::slice::from_raw_parts(
                        original.p_wait_semaphores,
                        original.wait_semaphore_count as usize,
                    )
                }
            };
            let copy_ready = { SWAPCHAIN.get_mut(&swapchain) }.and_then(|mut state| {
                if state.copy_engine.is_none() {
                    let ring = state.ring.take()?;
                    let timeline = state.timeline.take()?;
                    match SwapchainCopyEngine::create(
                        queue_state.dispatch.capture.as_ref()?.device.clone(),
                        ring,
                        timeline,
                        state.images.clone(),
                        state.extent,
                        queue_state.queue_family_index,
                        queue_state
                            .dispatch
                            .capture
                            .as_ref()
                            .and_then(|capture| capture.set_loader_data),
                    ) {
                        Ok(engine) => state.copy_engine = Some(engine),
                        Err(error) => {
                            layer_log(format!("snowglass: copy engine creation failed: {error}"));
                            return None;
                        }
                    }
                }
                let presented_image = state.images.get(image_index as usize).copied()?;
                let capture_rect = crate::scissor::capture_rect(presented_image, state.extent);
                let rect_is_content = capture_rect.is_some_and(|rect| rect.extent != state.extent);
                let presentation_source = (crate::scissor::tracking_enabled() && !rect_is_content)
                    .then(|| crate::tracking::render_target(presented_image))
                    .flatten();
                let render_extent = presentation_source.map_or_else(
                    || capture_rect.map_or(state.extent, |rect| rect.extent),
                    |candidate| candidate.extent,
                );
                let submission = if rect_is_content {
                    let rect = capture_rect?;
                    state.copy_engine.as_mut()?.submit_region_copy(
                        queue,
                        image_index,
                        rect.offset,
                        rect.extent,
                        waits,
                    )
                } else if let Some(candidate) = presentation_source {
                    state.copy_engine.as_mut()?.submit_image_copy(
                        queue,
                        image_index,
                        candidate.image,
                        candidate.extent,
                        candidate.layout,
                        waits,
                    )
                } else {
                    state
                        .copy_engine
                        .as_mut()?
                        .submit_copy(queue, image_index, waits)
                };
                match submission {
                    Ok(submission) => {
                        let count = COPIES_SUBMITTED.fetch_add(1, Ordering::Relaxed) + 1;
                        state.frame_number += 1;
                        let notification = Message::FrameReady(FrameReady {
                            stream_id: StreamId(swapchain.as_raw()),
                            slot: submission.slot,
                            render_width: render_extent.width,
                            render_height: render_extent.height,
                            capture_x: capture_rect.map_or(0, |rect| rect.offset.x as u32),
                            capture_y: capture_rect.map_or(0, |rect| rect.offset.y as u32),
                            capture_width: capture_rect
                                .map_or(state.extent.width, |r| r.extent.width),
                            capture_height: capture_rect
                                .map_or(state.extent.height, |r| r.extent.height),
                            frame_number: state.frame_number,
                            sync_value: submission.sync_value,
                            timestamp_qpc: performance_counter().unwrap_or(0),
                        });
                        if state
                            .ipc
                            .as_mut()
                            .is_some_and(|socket| socket.send(notification).is_err())
                        {
                            layer_log("snowglass: viewer socket disconnected");
                            state.ipc = None;
                        }
                        if count == 1 || count % 120 == 0 {
                            layer_log(format!(
                                "snowglass: submitted capture {}x{} (count={count}, cropped={})",
                                render_extent.width,
                                render_extent.height,
                                rect_is_content || presentation_source.is_some()
                            ));
                        }
                        Some(submission.present_ready)
                    }
                    Err(error) => {
                        layer_log(format!("snowglass: swapchain copy failed: {error}"));
                        None
                    }
                }
            });

            if let Some(copy_ready) = copy_ready {
                let copy_waits = [copy_ready];
                let mut modified = *original;
                modified.wait_semaphore_count = 1;
                modified.p_wait_semaphores = copy_waits.as_ptr();
                unsafe { continuation(queue, &modified, next) }
            } else {
                unsafe { continuation(queue, present_info, next) }
            }
        } else {
            unsafe { continuation(queue, present_info, next) }
        }
    } else {
        vk::Result::ERROR_DEVICE_LOST
    };
    let count = PRESENTS_SEEN.fetch_add(1, Ordering::Relaxed) + 1;
    if count == 1 || count % 120 == 0 {
        layer_log(format!(
            "snowglass: intercepted vkQueuePresentKHR (count={count})"
        ));
    }
    result
}

pub(crate) fn queue_submit(
    queue: vk::Queue,
    submit_count: u32,
    submits: *const vk::SubmitInfo<'_>,
    result: vk::Result,
) {
    let Some(queue_state) = ({ QUEUE.get(&queue).map(|entry| entry.clone()) }) else {
        return;
    };
    if result != vk::Result::SUCCESS || submit_count == 0 || submits.is_null() {
        return;
    }
    if crate::scissor::tracking_enabled() {
        return;
    }

    let mut selected: Option<(vk::SwapchainKHR, RenderTargetCandidate)> = None;
    for submit in unsafe { std::slice::from_raw_parts(submits, submit_count as usize) } {
        if submit.command_buffer_count == 0 || submit.p_command_buffers.is_null() {
            continue;
        }
        for command in unsafe {
            std::slice::from_raw_parts(
                submit.p_command_buffers,
                submit.command_buffer_count as usize,
            )
        } {
            let Some(candidate) = crate::tracking::command_candidate(*command) else {
                continue;
            };
            let Some(swapchain) = ({ SWAPCHAIN_IMAGE.get(&candidate.0).map(|entry| *entry) })
            else {
                continue;
            };
            if selected.is_none_or(|old| candidate.1.is_better_than(old.1)) {
                selected = Some((swapchain, candidate.1));
            }
        }
    }

    if let Some((swapchain, candidate)) = selected
        && let Some(mut state) = { SWAPCHAIN.get_mut(&swapchain) }
    {
        if state.copy_engine.is_none()
            && let (Some(ring), Some(timeline)) = (state.ring.take(), state.timeline.take())
        {
            let Some(capture) = queue_state.dispatch.capture.as_ref() else {
                return;
            };
            match SwapchainCopyEngine::create(
                capture.device.clone(),
                ring,
                timeline,
                state.images.clone(),
                state.extent,
                queue_state.queue_family_index,
                queue_state
                    .dispatch
                    .capture
                    .as_ref()
                    .and_then(|capture| capture.set_loader_data),
            ) {
                Ok(engine) => state.copy_engine = Some(engine),
                Err(error) => layer_log(format!(
                    "snowglass: tracked copy engine creation failed: {error}"
                )),
            }
        }
        if let Some(engine) = state.copy_engine.as_mut() {
            match engine.submit_tracked_copy(
                queue,
                candidate.image,
                candidate.extent,
                candidate.layout,
            ) {
                Ok(submission) => {
                    let count = COPIES_SUBMITTED.fetch_add(1, Ordering::Relaxed) + 1;
                    state.frame_number += 1;
                    let notification = Message::FrameReady(FrameReady {
                        stream_id: StreamId(swapchain.as_raw()),
                        slot: submission.slot,
                        render_width: candidate.extent.width,
                        render_height: candidate.extent.height,
                        capture_x: 0,
                        capture_y: 0,
                        capture_width: candidate.extent.width,
                        capture_height: candidate.extent.height,
                        frame_number: state.frame_number,
                        sync_value: submission.sync_value,
                        timestamp_qpc: performance_counter().unwrap_or(0),
                    });
                    if state
                        .ipc
                        .as_mut()
                        .is_some_and(|socket| socket.send(notification).is_err())
                    {
                        state.ipc = None;
                    }
                    if count == 1 || count % 120 == 0 {
                        layer_log(format!(
                            "snowglass: submitted graphics-queue capture {}x{} (count={count})",
                            candidate.extent.width, candidate.extent.height
                        ));
                    }
                }
                Err(error) => layer_log(format!("snowglass: tracked copy failed: {error}")),
            }
        }
    }
}

pub(crate) fn create_swapchain(
    device: vk::Device,
    create_info: *const vk::SwapchainCreateInfoKHR<'_>,
    swapchain: *mut vk::SwapchainKHR,
    result: vk::Result,
) {
    let dispatch =
        unsafe { dispatch_key(device) }.and_then(|key| DEVICE.get(&key).map(|entry| entry.clone()));
    let mut modified_create_info = (!create_info.is_null()).then(|| unsafe { *create_info });
    if let Some(info) = modified_create_info.as_mut() {
        if dispatch
            .as_ref()
            .is_some_and(|entry| entry.capture.is_some())
        {
            info.image_usage |= vk::ImageUsageFlags::TRANSFER_SRC;
        }
    }
    if result == vk::Result::SUCCESS {
        let count = SWAPCHAINS_CREATED.fetch_add(1, Ordering::Relaxed) + 1;
        if !create_info.is_null() && !swapchain.is_null() {
            let info = modified_create_info
                .as_ref()
                .unwrap_or_else(|| unsafe { &*create_info });
            let ring = dispatch
                .as_ref()
                .and_then(|entry| entry.capture.as_ref())
                .and_then(|capture| {
                    match ExportImageRing::create_with_function(
                        capture.device.clone(),
                        capture.get_memory_win32_handle,
                        &capture.memory_properties,
                        ExportImageRingDesc {
                            extent: info.image_extent,
                            format: info.image_format,
                            slots: 3,
                        },
                    ) {
                        Ok(ring) => {
                            let handles = ring
                                .slots()
                                .iter()
                                .map(|slot| slot.raw_handle())
                                .collect::<Vec<_>>();
                            layer_log(format!(
                                "snowglass: attached {}-slot export ring {:?}",
                                ring.slots().len(),
                                handles
                            ));
                            Some(ring)
                        }
                        Err(error) => {
                            layer_log(format!("snowglass: export ring allocation failed: {error}"));
                            None
                        }
                    }
                });
            let timeline = ring.as_ref().and_then(|_| {
                let capture = dispatch.as_ref()?.capture.as_ref()?;
                match ExportTimelineSemaphore::create_with_function(
                    capture.device.clone(),
                    capture.get_semaphore_win32_handle,
                ) {
                    Ok(timeline) => Some(timeline),
                    Err(error) => {
                        layer_log(format!(
                            "snowglass: export timeline semaphore creation failed: {error}"
                        ));
                        None
                    }
                }
            });
            let ipc = ring
                .as_ref()
                .zip(timeline.as_ref())
                .and_then(|(ring, timeline)| {
                    let source_window = {
                        SURFACE_WINDOW
                            .get(&info.surface)
                            .map(|entry| *entry)
                            .unwrap_or(0)
                    };
                    connect_viewer(
                        ring,
                        timeline,
                        info.image_extent,
                        info.image_format,
                        source_window,
                    )
                });
            unsafe {
                SWAPCHAIN.insert(
                    *swapchain,
                    SwapchainState {
                        device,
                        format: info.image_format,
                        extent: info.image_extent,
                        images: Vec::new(),
                        ring,
                        timeline,
                        copy_engine: None,
                        ipc,
                        frame_number: 0,
                    },
                );
            }
        }
        layer_log(format!(
            "snowglass: intercepted vkCreateSwapchainKHR (count={count})"
        ));
    }
}

pub(crate) fn destroy_swapchain(swapchain: vk::SwapchainKHR) {
    if let Some(state) = { SWAPCHAIN.get(&swapchain) } {
        for image in &state.images {
            {
                SWAPCHAIN_IMAGE.remove(image);
                crate::tracking::remove_render_target(*image);
                crate::scissor::remove(*image);
            }
        }
    }
    {
        SWAPCHAIN.remove(&swapchain)
    };
}

pub(crate) fn get_swapchain_images(
    swapchain: vk::SwapchainKHR,
    image_count: *mut u32,
    images: *mut vk::Image,
    result: vk::Result,
) {
    if matches!(result, vk::Result::SUCCESS | vk::Result::INCOMPLETE)
        && !image_count.is_null()
        && !images.is_null()
    {
        let captured =
            unsafe { std::slice::from_raw_parts(images, *image_count as usize).to_vec() };
        if let Some(mut state) = { SWAPCHAIN.get_mut(&swapchain) } {
            state.images = captured;
            if crate::scissor::tracking_enabled() {
                for image in &state.images {
                    {
                        SWAPCHAIN_IMAGE.insert(*image, swapchain)
                    };
                }
            }
            layer_log(format!(
                "snowglass: associated {} swapchain images ({}x{}, format={})",
                state.images.len(),
                state.extent.width,
                state.extent.height,
                state.format.as_raw()
            ));
        }
    }
}
