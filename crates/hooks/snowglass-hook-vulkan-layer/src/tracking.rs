//! Render-target tracking observers.

use ash::vk;
use dashmap::DashMap;
use std::sync::LazyLock;

use crate::layer::*;

#[derive(Clone, Copy)]
pub(crate) struct RenderTargetCandidate {
    pub(crate) image: vk::Image,
    pub(crate) extent: vk::Extent2D,
    pub(crate) layout: vk::ImageLayout,
    score: u64,
    cohort_size: u32,
}

static RENDER_TARGET: LazyLock<DashMap<vk::Image, RenderTargetCandidate>> =
    LazyLock::new(Default::default);

#[derive(Clone, Default)]
struct CommandTrackingState {
    framebuffer: Option<vk::Framebuffer>,
    descriptor_sets: Vec<vk::DescriptorSet>,
    pushed_images: Vec<(vk::Image, vk::ImageLayout)>,
    viewport: Option<vk::Viewport>,
    scissor: Option<vk::Rect2D>,
}

static IMAGE_EXTENT: LazyLock<DashMap<vk::Image, vk::Extent2D>> = LazyLock::new(Default::default);
static IMAGE_INFO: LazyLock<DashMap<vk::Image, (vk::Format, vk::ImageUsageFlags)>> =
    LazyLock::new(Default::default);
static IMAGE_VIEW: LazyLock<DashMap<vk::ImageView, vk::Image>> = LazyLock::new(Default::default);
static FRAMEBUFFER_IMAGES: LazyLock<DashMap<vk::Framebuffer, Vec<vk::Image>>> =
    LazyLock::new(Default::default);
static DESCRIPTOR_IMAGES: LazyLock<
    DashMap<(vk::DescriptorSet, u32, u32), (vk::Image, vk::ImageLayout)>,
> = LazyLock::new(Default::default);
static COMMAND_TRACKING: LazyLock<DashMap<vk::CommandBuffer, CommandTrackingState>> =
    LazyLock::new(Default::default);
static COMMAND_CANDIDATE: LazyLock<DashMap<vk::CommandBuffer, (vk::Image, RenderTargetCandidate)>> =
    LazyLock::new(Default::default);

pub(crate) fn command_candidate(
    command_buffer: vk::CommandBuffer,
) -> Option<(vk::Image, RenderTargetCandidate)> {
    COMMAND_CANDIDATE.get(&command_buffer).map(|entry| *entry)
}

pub(crate) fn render_target(image: vk::Image) -> Option<RenderTargetCandidate> {
    RENDER_TARGET.get(&image).map(|entry| *entry)
}

pub(crate) fn remove_render_target(image: vk::Image) {
    {
        RENDER_TARGET.remove(&image)
    };
}

impl RenderTargetCandidate {
    pub(crate) fn is_better_than(self, other: Self) -> bool {
        (self.score, self.cohort_size) > (other.score, other.cohort_size)
    }
}

fn image_cohort_size(image: vk::Image, extent: vk::Extent2D) -> u32 {
    let Some(info) = ({ IMAGE_INFO.get(&image).map(|entry| *entry) }) else {
        return 0;
    };
    {
        IMAGE_INFO
            .iter()
            .filter(|entry| {
                *entry.value() == info
                    && IMAGE_EXTENT
                        .get(entry.key())
                        .is_some_and(|candidate| *candidate == extent)
            })
            .count() as u32
    }
}

pub(crate) fn noop() {}

/// The draw observer deliberately declares only the dispatchable command
/// buffer. Vertex topology is irrelevant to capture target selection.
pub(crate) fn cmd_draw(command_buffer: vk::CommandBuffer) {
    let Some(state) = ({
        COMMAND_TRACKING
            .get(&command_buffer)
            .map(|entry| entry.clone())
    }) else {
        return;
    };
    let Some(framebuffer) = state.framebuffer else {
        return;
    };
    let Some(images) = ({ FRAMEBUFFER_IMAGES.get(&framebuffer) }) else {
        return;
    };
    let Some(destination) = images
        .iter()
        .find(|image| crate::swapchain::is_swapchain_image(**image))
        .copied()
    else {
        return;
    };
    let draw_area = state
        .scissor
        .map(|rect| u64::from(rect.extent.width) * u64::from(rect.extent.height))
        .or_else(|| {
            state
                .viewport
                .map(|vp| (vp.width.abs() as u64) * (vp.height.abs() as u64))
        })
        .unwrap_or(0);
    if draw_area == 0 {
        return;
    }
    let mut best = None;
    for entry in DESCRIPTOR_IMAGES.iter() {
        let ((set, _, _), (image, layout)) = (*entry.key(), *entry.value());
        if !state.descriptor_sets.contains(&set) || image == destination {
            continue;
        }
        let Some(extent) = ({ IMAGE_EXTENT.get(&image).map(|entry| *entry) }) else {
            continue;
        };
        let pixels = u64::from(extent.width) * u64::from(extent.height);
        if pixels < 64 * 64 {
            continue;
        }
        if best.is_none_or(|candidate: RenderTargetCandidate| {
            let cohort = image_cohort_size(image, extent);
            cohort > candidate.cohort_size
                || (cohort == candidate.cohort_size
                    && pixels
                        > u64::from(candidate.extent.width) * u64::from(candidate.extent.height))
        }) {
            best = Some(RenderTargetCandidate {
                image,
                extent,
                layout,
                score: draw_area,
                cohort_size: image_cohort_size(image, extent),
            });
        }
    }
    for &(image, layout) in &state.pushed_images {
        if image == destination {
            continue;
        }
        let Some(extent) = ({ IMAGE_EXTENT.get(&image).map(|entry| *entry) }) else {
            continue;
        };
        let pixels = u64::from(extent.width) * u64::from(extent.height);
        if pixels < 64 * 64 {
            continue;
        }
        let cohort = image_cohort_size(image, extent);
        if best.is_none_or(|candidate: RenderTargetCandidate| {
            cohort > candidate.cohort_size
                || (cohort == candidate.cohort_size
                    && pixels
                        > u64::from(candidate.extent.width) * u64::from(candidate.extent.height))
        }) {
            best = Some(RenderTargetCandidate {
                image,
                extent,
                layout,
                score: draw_area,
                cohort_size: cohort,
            });
        }
    }
    if let Some(candidate) = best {
        let rect = state.scissor.or_else(|| {
            state.viewport.map(|viewport| vk::Rect2D {
                offset: vk::Offset2D {
                    x: viewport.x.max(0.0).round() as i32,
                    y: viewport.y.max(0.0).round() as i32,
                },
                extent: vk::Extent2D {
                    width: viewport.width.abs().round() as u32,
                    height: viewport.height.abs().round() as u32,
                },
            })
        });
        if let Some(rect) = rect {
            crate::scissor::record(destination, rect, draw_area);
        }
        let replace = {
            RENDER_TARGET
                .get(&destination)
                .is_none_or(|old| candidate.is_better_than(*old))
        };
        if replace {
            {
                RENDER_TARGET.insert(destination, candidate)
            };
        }
        let replace_command = {
            COMMAND_CANDIDATE
                .get(&command_buffer)
                .is_none_or(|old| candidate.is_better_than(old.1))
        };
        if replace_command {
            {
                COMMAND_CANDIDATE.insert(command_buffer, (destination, candidate))
            };
        }
    }
}

pub(crate) fn begin_command_buffer(command_buffer: vk::CommandBuffer) {
    {
        COMMAND_TRACKING.remove(&command_buffer);
        COMMAND_CANDIDATE.remove(&command_buffer);
    }
}

pub(crate) fn create_image(
    create_info: *const vk::ImageCreateInfo<'_>,
    image: *mut vk::Image,
    result: vk::Result,
) {
    if result == vk::Result::SUCCESS && !create_info.is_null() && !image.is_null() {
        let info = unsafe { &*create_info };
        unsafe {
            IMAGE_EXTENT.insert(
                *image,
                vk::Extent2D {
                    width: info.extent.width,
                    height: info.extent.height,
                },
            );
            IMAGE_INFO.insert(
                *image,
                (info.format, info.usage | vk::ImageUsageFlags::TRANSFER_SRC),
            );
        }
    }
}

pub(crate) fn create_image_view(
    create_info: *const vk::ImageViewCreateInfo<'_>,
    view: *mut vk::ImageView,
    result: vk::Result,
) {
    if result == vk::Result::SUCCESS && !create_info.is_null() && !view.is_null() {
        unsafe {
            IMAGE_VIEW.insert(*view, (*create_info).image);
        }
    }
}

pub(crate) fn create_framebuffer(
    create_info: *const vk::FramebufferCreateInfo<'_>,
    framebuffer: *mut vk::Framebuffer,
    result: vk::Result,
) {
    if result != vk::Result::SUCCESS || create_info.is_null() || framebuffer.is_null() {
        return;
    }
    let info = unsafe { &*create_info };
    let views = if info.attachment_count == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(info.p_attachments, info.attachment_count as usize) }
    };
    let images = views
        .iter()
        .filter_map(|view| IMAGE_VIEW.get(view).map(|entry| *entry))
        .collect();
    unsafe {
        FRAMEBUFFER_IMAGES.insert(*framebuffer, images);
    }
}

pub(crate) fn destroy_image(image: vk::Image) {
    {
        IMAGE_EXTENT.remove(&image);
        IMAGE_INFO.remove(&image);
    }
}

pub(crate) fn destroy_image_view(view: vk::ImageView) {
    {
        IMAGE_VIEW.remove(&view);
    }
}

pub(crate) fn destroy_framebuffer(framebuffer: vk::Framebuffer) {
    {
        FRAMEBUFFER_IMAGES.remove(&framebuffer);
    }
}

pub(crate) fn update_descriptor_sets(write_count: u32, writes: *const vk::WriteDescriptorSet<'_>) {
    if write_count == 0 || writes.is_null() {
        return;
    }
    for write in unsafe { std::slice::from_raw_parts(writes, write_count as usize) } {
        if !matches!(
            write.descriptor_type,
            vk::DescriptorType::COMBINED_IMAGE_SAMPLER | vk::DescriptorType::SAMPLED_IMAGE
        ) || write.p_image_info.is_null()
        {
            continue;
        }
        for (index, info) in unsafe {
            std::slice::from_raw_parts(write.p_image_info, write.descriptor_count as usize)
        }
        .iter()
        .enumerate()
        {
            if let Some(image) = { IMAGE_VIEW.get(&info.image_view).map(|entry| *entry) } {
                {
                    DESCRIPTOR_IMAGES.insert(
                        (
                            write.dst_set,
                            write.dst_binding,
                            write.dst_array_element + index as u32,
                        ),
                        (image, info.image_layout),
                    );
                }
            }
        }
    }
}

pub(crate) fn get_device_queue(device: vk::Device, queue_family_index: u32, queue: *mut vk::Queue) {
    let Some(dispatch) = (unsafe { dispatch_key(device) })
        .and_then(|key| DEVICE.get(&key).map(|entry| entry.clone()))
    else {
        return;
    };
    if !queue.is_null() {
        let queue = unsafe { *queue };
        if queue != vk::Queue::default() {
            {
                QUEUE.insert(
                    queue,
                    QueueDispatchTable {
                        device,
                        queue_family_index,
                        dispatch,
                    },
                )
            };
        }
    }
}

pub(crate) fn get_device_queue2(
    device: vk::Device,
    queue_info: *const vk::DeviceQueueInfo2<'_>,
    queue: *mut vk::Queue,
) {
    let Some(dispatch) = (unsafe { dispatch_key(device) })
        .and_then(|key| DEVICE.get(&key).map(|entry| entry.clone()))
    else {
        return;
    };
    if !queue.is_null() {
        let queue = unsafe { *queue };
        if queue != vk::Queue::default() {
            let queue_family_index = if queue_info.is_null() {
                0
            } else {
                unsafe { (*queue_info).queue_family_index }
            };
            {
                QUEUE.insert(
                    queue,
                    QueueDispatchTable {
                        device,
                        queue_family_index,
                        dispatch,
                    },
                )
            };
        }
    }
}

pub(crate) fn enumerate_physical_devices(
    instance: vk::Instance,
    physical_device_count: *mut u32,
    physical_devices: *mut vk::PhysicalDevice,
    result: vk::Result,
) {
    let Some(next_gipa) = instance_gipa(instance) else {
        return;
    };
    if matches!(result, vk::Result::SUCCESS | vk::Result::INCOMPLETE)
        && !physical_device_count.is_null()
        && !physical_devices.is_null()
    {
        for physical_device in
            unsafe { std::slice::from_raw_parts(physical_devices, *physical_device_count as usize) }
        {
            {
                PHYSICAL_DEVICE.insert(
                    *physical_device,
                    PhysicalDeviceState {
                        instance,
                        next_gipa,
                    },
                );
            }
        }
    }
}

pub(crate) fn create_instance(result: vk::Result) {
    layer_log(format!(
        "snowglass: downstream vkCreateInstance returned {result}"
    ));
}

pub(crate) fn cmd_set_viewport(
    command_buffer: vk::CommandBuffer,
    first: u32,
    count: u32,
    viewports: *const vk::Viewport,
) {
    if first == 0 && count != 0 && !viewports.is_null() {
        let viewport = unsafe { *viewports };
        COMMAND_TRACKING.entry(command_buffer).or_default().viewport = Some(viewport);
    }
}

pub(crate) fn cmd_set_scissor(
    command_buffer: vk::CommandBuffer,
    first: u32,
    count: u32,
    scissors: *const vk::Rect2D,
) {
    if first == 0 && count != 0 && !scissors.is_null() {
        let scissor = unsafe { *scissors };
        COMMAND_TRACKING.entry(command_buffer).or_default().scissor = Some(scissor);
    }
}

pub(crate) fn cmd_end_render_pass(command_buffer: vk::CommandBuffer) {
    if let Some(mut state) = { COMMAND_TRACKING.get_mut(&command_buffer) } {
        state.framebuffer = None;
    }
}

pub(crate) fn cmd_begin_render_pass(
    command_buffer: vk::CommandBuffer,
    begin: *const vk::RenderPassBeginInfo<'_>,
    contents: vk::SubpassContents,
) {
    let _ = contents;
    if !begin.is_null() {
        unsafe {
            COMMAND_TRACKING
                .entry(command_buffer)
                .or_default()
                .framebuffer = Some((*begin).framebuffer)
        };
    }
}

pub(crate) fn cmd_bind_descriptor_sets(
    command_buffer: vk::CommandBuffer,
    bind_point: vk::PipelineBindPoint,
    layout: vk::PipelineLayout,
    first_set: u32,
    set_count: u32,
    sets: *const vk::DescriptorSet,
    dynamic_offset_count: u32,
    dynamic_offsets: *const u32,
) {
    let _ = (layout, dynamic_offset_count, dynamic_offsets);
    if bind_point != vk::PipelineBindPoint::GRAPHICS || set_count == 0 || sets.is_null() {
        return;
    }
    let bound = unsafe { std::slice::from_raw_parts(sets, set_count as usize) };
    let mut state = { COMMAND_TRACKING.entry(command_buffer).or_default() };
    let required = first_set as usize + bound.len();
    state
        .descriptor_sets
        .resize(required, vk::DescriptorSet::null());
    state.descriptor_sets[first_set as usize..required].copy_from_slice(bound);
}

pub(crate) fn cmd_push_descriptor_set(
    command_buffer: vk::CommandBuffer,
    bind_point: vk::PipelineBindPoint,
    layout: vk::PipelineLayout,
    set: u32,
    write_count: u32,
    writes: *const vk::WriteDescriptorSet<'_>,
) {
    let _ = (bind_point, layout, set);
    if write_count == 0 || writes.is_null() {
        return;
    }
    let mut pushed_images = Vec::new();
    for write in unsafe { std::slice::from_raw_parts(writes, write_count as usize) } {
        if !matches!(
            write.descriptor_type,
            vk::DescriptorType::COMBINED_IMAGE_SAMPLER | vk::DescriptorType::SAMPLED_IMAGE
        ) || write.p_image_info.is_null()
        {
            continue;
        }
        for info in unsafe {
            std::slice::from_raw_parts(write.p_image_info, write.descriptor_count as usize)
        } {
            if let Some(image) = { IMAGE_VIEW.get(&info.image_view).map(|entry| *entry) } {
                pushed_images.push((image, info.image_layout));
            }
        }
    }
    {
        COMMAND_TRACKING
            .entry(command_buffer)
            .or_default()
            .pushed_images = pushed_images
    };
}

pub(crate) fn cmd_blit_image(
    command_buffer: vk::CommandBuffer,
    source_image: vk::Image,
    source_layout: vk::ImageLayout,
    destination_image: vk::Image,
    destination_layout: vk::ImageLayout,
    region_count: u32,
    regions: *const vk::ImageBlit,
    filter: vk::Filter,
) {
    let _ = (command_buffer, destination_layout, filter);
    if region_count != 0
        && !regions.is_null()
        && source_image != destination_image
        && crate::swapchain::is_swapchain_image(destination_image)
    {
        let region = unsafe { &*regions };
        let width = (region.src_offsets[1].x - region.src_offsets[0].x).unsigned_abs();
        let height = (region.src_offsets[1].y - region.src_offsets[0].y).unsigned_abs();
        if width != 0 && height != 0 {
            {
                RENDER_TARGET.insert(
                    destination_image,
                    RenderTargetCandidate {
                        image: source_image,
                        extent: vk::Extent2D { width, height },
                        layout: source_layout,
                        score: u64::MAX,
                        cohort_size: u32::MAX,
                    },
                )
            };
        }
    }
}

pub(crate) fn cmd_copy_image(
    command_buffer: vk::CommandBuffer,
    source_image: vk::Image,
    source_layout: vk::ImageLayout,
    destination_image: vk::Image,
    destination_layout: vk::ImageLayout,
    region_count: u32,
    regions: *const vk::ImageCopy,
) {
    let _ = (command_buffer, destination_layout);
    if region_count != 0
        && !regions.is_null()
        && source_image != destination_image
        && crate::swapchain::is_swapchain_image(destination_image)
    {
        let extent = unsafe { (*regions).extent };
        if extent.width != 0 && extent.height != 0 {
            {
                RENDER_TARGET.insert(
                    destination_image,
                    RenderTargetCandidate {
                        image: source_image,
                        extent: vk::Extent2D {
                            width: extent.width,
                            height: extent.height,
                        },
                        layout: source_layout,
                        score: u64::MAX,
                        cohort_size: u32::MAX,
                    },
                )
            };
        }
    }
}
