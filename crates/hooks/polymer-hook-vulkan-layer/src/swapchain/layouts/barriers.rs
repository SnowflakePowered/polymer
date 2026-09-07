use super::*;
impl Device {
    fn barrier(&self, barrier: &mut vk::ImageMemoryBarrier<'_>) {
        if !self.images.contains_key(&barrier.image) {
            return;
        }
        barrier.old_layout = Layouts::layout(barrier.old_layout);
        barrier.new_layout = Layouts::layout(barrier.new_layout);
        barrier.src_queue_family_index = vk::QUEUE_FAMILY_IGNORED;
        barrier.dst_queue_family_index = vk::QUEUE_FAMILY_IGNORED;
    }
    fn barrier2(&self, barrier: &mut vk::ImageMemoryBarrier2<'_>) {
        if !self.images.contains_key(&barrier.image) {
            return;
        }
        barrier.old_layout = Layouts::layout(barrier.old_layout);
        barrier.new_layout = Layouts::layout(barrier.new_layout);
        barrier.src_queue_family_index = vk::QUEUE_FAMILY_IGNORED;
        barrier.dst_queue_family_index = vk::QUEUE_FAMILY_IGNORED;
    }
}
unsafe extern "system" fn pipeline(
    command: vk::CommandBuffer,
    src: vk::PipelineStageFlags,
    dst: vk::PipelineStageFlags,
    flags: vk::DependencyFlags,
    memory_count: u32,
    memory: *const vk::MemoryBarrier<'_>,
    buffer_count: u32,
    buffers: *const vk::BufferMemoryBarrier<'_>,
    image_count: u32,
    images: *const vk::ImageMemoryBarrier<'_>,
) {
    let Some((gdpa, handle)) = crate::layer::command_dispatch_data(command) else {
        return;
    };
    let Some(next): Option<vk::PFN_vkCmdPipelineBarrier> =
        (unsafe { crate::terminal::device_proc(gdpa, handle, c"vkCmdPipelineBarrier") })
    else {
        return;
    };
    let mut translated = unsafe { Layouts::slice(images, image_count) }.to_vec();
    if let Ok(device) = Device::get(handle) {
        for barrier in &mut translated {
            device.barrier(barrier);
        }
    }
    unsafe {
        next(
            command,
            src,
            dst,
            flags,
            memory_count,
            memory,
            buffer_count,
            buffers,
            image_count,
            translated.as_ptr(),
        );
    }
}
unsafe extern "system" fn wait(
    command: vk::CommandBuffer,
    count: u32,
    events: *const vk::Event,
    src: vk::PipelineStageFlags,
    dst: vk::PipelineStageFlags,
    memory_count: u32,
    memory: *const vk::MemoryBarrier<'_>,
    buffer_count: u32,
    buffers: *const vk::BufferMemoryBarrier<'_>,
    image_count: u32,
    images: *const vk::ImageMemoryBarrier<'_>,
) {
    let Some((gdpa, handle)) = crate::layer::command_dispatch_data(command) else {
        return;
    };
    let Some(next): Option<vk::PFN_vkCmdWaitEvents> =
        (unsafe { crate::terminal::device_proc(gdpa, handle, c"vkCmdWaitEvents") })
    else {
        return;
    };
    let mut translated = unsafe { Layouts::slice(images, image_count) }.to_vec();
    if let Ok(device) = Device::get(handle) {
        for barrier in &mut translated {
            device.barrier(barrier);
        }
    }
    unsafe {
        next(
            command,
            count,
            events,
            src,
            dst,
            memory_count,
            memory,
            buffer_count,
            buffers,
            image_count,
            translated.as_ptr(),
        );
    }
}
struct Dependency<'a> {
    info: vk::DependencyInfo<'a>,
    images: Vec<vk::ImageMemoryBarrier2<'a>>,
}
impl<'a> Dependency<'a> {
    unsafe fn new(device: Option<&Device>, info: &vk::DependencyInfo<'a>) -> Self {
        let mut images = unsafe {
            Layouts::slice(
                info.p_image_memory_barriers,
                info.image_memory_barrier_count,
            )
        }
        .to_vec();
        if let Some(device) = device {
            for barrier in &mut images {
                device.barrier2(barrier);
            }
        }
        Self {
            info: *info,
            images,
        }
    }
    fn info(&self) -> vk::DependencyInfo<'a> {
        vk::DependencyInfo {
            p_image_memory_barriers: self.images.as_ptr(),
            ..self.info
        }
    }
}
unsafe extern "system" fn pipeline2(
    command: vk::CommandBuffer,
    info: *const vk::DependencyInfo<'_>,
) {
    let Some((gdpa, handle)) = crate::layer::command_dispatch_data(command) else {
        return;
    };
    let Some(next): Option<vk::PFN_vkCmdPipelineBarrier2> = (unsafe {
        crate::terminal::device_proc(gdpa, handle, c"vkCmdPipelineBarrier2")
            .or_else(|| crate::terminal::device_proc(gdpa, handle, c"vkCmdPipelineBarrier2KHR"))
    }) else {
        return;
    };
    let device = Device::get(handle).ok();
    let translated = unsafe { Dependency::new(device.as_deref(), &*info) };
    unsafe {
        next(command, &translated.info());
    }
}
unsafe extern "system" fn set2(
    command: vk::CommandBuffer,
    event: vk::Event,
    info: *const vk::DependencyInfo<'_>,
) {
    let Some((gdpa, handle)) = crate::layer::command_dispatch_data(command) else {
        return;
    };
    let Some(next): Option<vk::PFN_vkCmdSetEvent2> = (unsafe {
        crate::terminal::device_proc(gdpa, handle, c"vkCmdSetEvent2")
            .or_else(|| crate::terminal::device_proc(gdpa, handle, c"vkCmdSetEvent2KHR"))
    }) else {
        return;
    };
    let device = Device::get(handle).ok();
    let translated = unsafe { Dependency::new(device.as_deref(), &*info) };
    unsafe {
        next(command, event, &translated.info());
    }
}
unsafe extern "system" fn wait2(
    command: vk::CommandBuffer,
    count: u32,
    events: *const vk::Event,
    infos: *const vk::DependencyInfo<'_>,
) {
    let Some((gdpa, handle)) = crate::layer::command_dispatch_data(command) else {
        return;
    };
    let Some(next): Option<vk::PFN_vkCmdWaitEvents2> = (unsafe {
        crate::terminal::device_proc(gdpa, handle, c"vkCmdWaitEvents2")
            .or_else(|| crate::terminal::device_proc(gdpa, handle, c"vkCmdWaitEvents2KHR"))
    }) else {
        return;
    };
    let device = Device::get(handle).ok();
    let translated = unsafe { Layouts::slice(infos, count) }
        .iter()
        .map(|info| unsafe { Dependency::new(device.as_deref(), info) })
        .collect::<Vec<_>>();
    let infos = translated.iter().map(Dependency::info).collect::<Vec<_>>();
    unsafe {
        next(command, count, events, infos.as_ptr());
    }
}
pub(super) fn hook(name: &[u8]) -> vk::PFN_vkVoidFunction {
    crate::hook(match name {
        b"vkCmdPipelineBarrier" => pipeline as *const (),
        b"vkCmdWaitEvents" => wait as *const (),
        b"vkCmdPipelineBarrier2" | b"vkCmdPipelineBarrier2KHR" => pipeline2 as *const (),
        b"vkCmdWaitEvents2" | b"vkCmdWaitEvents2KHR" => wait2 as *const (),
        b"vkCmdSetEvent2" | b"vkCmdSetEvent2KHR" => set2 as *const (),
        _ => return None,
    })
}
