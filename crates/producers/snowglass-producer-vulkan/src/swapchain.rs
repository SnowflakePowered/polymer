use crate::{ExportImageRing, ExportTimelineSemaphore};
use ash::{vk, vk::Handle};
use std::ffi::c_void;

pub type SetDeviceLoaderData = unsafe extern "system" fn(vk::Device, *mut c_void) -> vk::Result;

pub struct CopySubmission {
    pub present_ready: vk::Semaphore,
    pub slot: u32,
    pub sync_value: u64,
}

struct CopySlot {
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
    initialized: bool,
}

/// Records and submits swapchain-to-export-ring copies on an application's
/// presentation queue.
pub struct SwapchainCopyEngine {
    device: ash::Device,
    ring: ExportImageRing,
    timeline: ExportTimelineSemaphore,
    source_images: Vec<vk::Image>,
    extent: vk::Extent2D,
    command_pool: vk::CommandPool,
    slots: Vec<CopySlot>,
    present_ready: Vec<vk::Semaphore>,
    next_slot: usize,
    next_sync_value: u64,
}

impl SwapchainCopyEngine {
    pub fn create(
        device: ash::Device,
        ring: ExportImageRing,
        timeline: ExportTimelineSemaphore,
        source_images: Vec<vk::Image>,
        extent: vk::Extent2D,
        queue_family_index: u32,
        set_loader_data: Option<SetDeviceLoaderData>,
    ) -> Result<Self, vk::Result> {
        if source_images.is_empty() || ring.slots().is_empty() {
            return Err(vk::Result::ERROR_INITIALIZATION_FAILED);
        }
        // SAFETY: all created resources are owned by the returned engine and
        // cleaned up on partial failure through the local cleanup path.
        unsafe {
            let pool_info = vk::CommandPoolCreateInfo::default()
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
                .queue_family_index(queue_family_index);
            let command_pool = device.create_command_pool(&pool_info, None)?;
            let allocation_info = vk::CommandBufferAllocateInfo::default()
                .command_pool(command_pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(ring.slots().len() as u32);
            let command_buffers = match device.allocate_command_buffers(&allocation_info) {
                Ok(command_buffers) => command_buffers,
                Err(error) => {
                    device.destroy_command_pool(command_pool, None);
                    return Err(error);
                }
            };
            if let Some(set_loader_data) = set_loader_data {
                for &command_buffer in &command_buffers {
                    let result = set_loader_data(
                        device.handle(),
                        command_buffer.as_raw() as usize as *mut c_void,
                    );
                    if result != vk::Result::SUCCESS {
                        device.destroy_command_pool(command_pool, None);
                        return Err(result);
                    }
                }
            }
            let mut slots = Vec::with_capacity(command_buffers.len());
            for command_buffer in command_buffers {
                let fence = match device.create_fence(
                    &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                    None,
                ) {
                    Ok(fence) => fence,
                    Err(error) => {
                        destroy_slots(&device, command_pool, &mut slots);
                        return Err(error);
                    }
                };
                slots.push(CopySlot {
                    command_buffer,
                    fence,
                    initialized: false,
                });
            }
            let mut present_ready = Vec::with_capacity(source_images.len());
            for _ in &source_images {
                match device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) {
                    Ok(semaphore) => present_ready.push(semaphore),
                    Err(error) => {
                        for semaphore in present_ready.drain(..) {
                            device.destroy_semaphore(semaphore, None);
                        }
                        destroy_slots(&device, command_pool, &mut slots);
                        return Err(error);
                    }
                }
            }
            Ok(Self {
                device,
                ring,
                timeline,
                source_images,
                extent,
                command_pool,
                slots,
                present_ready,
                next_slot: 0,
                next_sync_value: 1,
            })
        }
    }

    /// Submits one copy and returns the semaphore that presentation must wait on.
    pub fn submit_copy(
        &mut self,
        queue: vk::Queue,
        source_image_index: u32,
        application_waits: &[vk::Semaphore],
    ) -> Result<CopySubmission, vk::Result> {
        let Some(&source_image) = self.source_images.get(source_image_index as usize) else {
            return Err(vk::Result::ERROR_OUT_OF_DATE_KHR);
        };
        let ready = self.present_ready[source_image_index as usize];
        let slot_index = self.next_slot;
        self.next_slot = (self.next_slot + 1) % self.slots.len();
        let sync_value = self.next_sync_value;
        self.next_sync_value += 1;
        let slot = &mut self.slots[slot_index];
        let destination_image = self.ring.slots()[slot_index].image();

        // SAFETY: the slot fence protects command-buffer reuse; swapchain image
        // ownership remains on this queue and the caller supplies valid waits.
        unsafe {
            self.device.wait_for_fences(&[slot.fence], true, u64::MAX)?;
            self.device.reset_fences(&[slot.fence])?;
            self.device
                .reset_command_buffer(slot.command_buffer, vk::CommandBufferResetFlags::empty())?;
            self.device.begin_command_buffer(
                slot.command_buffer,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )?;

            let color_range = vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            };
            let source_to_transfer = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::MEMORY_READ)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                .old_layout(vk::ImageLayout::PRESENT_SRC_KHR)
                .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .image(source_image)
                .subresource_range(color_range);
            let destination_to_transfer = vk::ImageMemoryBarrier::default()
                .src_access_mask(if slot.initialized {
                    vk::AccessFlags::MEMORY_READ
                } else {
                    vk::AccessFlags::empty()
                })
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .old_layout(if slot.initialized {
                    vk::ImageLayout::GENERAL
                } else {
                    vk::ImageLayout::UNDEFINED
                })
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .image(destination_image)
                .subresource_range(color_range);
            self.device.cmd_pipeline_barrier(
                slot.command_buffer,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[source_to_transfer, destination_to_transfer],
            );
            let copy_region = vk::ImageCopy::default()
                .src_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .dst_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .extent(vk::Extent3D {
                    width: self.extent.width,
                    height: self.extent.height,
                    depth: 1,
                });
            self.device.cmd_copy_image(
                slot.command_buffer,
                source_image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                destination_image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[copy_region],
            );
            let source_to_present = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::TRANSFER_READ)
                .dst_access_mask(vk::AccessFlags::MEMORY_READ)
                .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
                .image(source_image)
                .subresource_range(color_range);
            let destination_to_general = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::MEMORY_READ)
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .image(destination_image)
                .subresource_range(color_range);
            self.device.cmd_pipeline_barrier(
                slot.command_buffer,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[source_to_present, destination_to_general],
            );
            self.device.end_command_buffer(slot.command_buffer)?;

            let wait_stages = vec![vk::PipelineStageFlags::TRANSFER; application_waits.len()];
            let command_buffers = [slot.command_buffer];
            let signals = [ready, self.timeline.semaphore()];
            let signal_values = [0, sync_value];
            let mut timeline_submit =
                vk::TimelineSemaphoreSubmitInfo::default().signal_semaphore_values(&signal_values);
            let submit = vk::SubmitInfo::default()
                .push_next(&mut timeline_submit)
                .wait_semaphores(application_waits)
                .wait_dst_stage_mask(&wait_stages)
                .command_buffers(&command_buffers)
                .signal_semaphores(&signals);
            self.device.queue_submit(queue, &[submit], slot.fence)?;
            slot.initialized = true;
            Ok(CopySubmission {
                present_ready: ready,
                slot: slot_index as u32,
                sync_value,
            })
        }
    }
}

impl Drop for SwapchainCopyEngine {
    fn drop(&mut self) {
        // The layer calls device_wait_idle before removing engines at device
        // destruction; swapchain destruction is externally synchronized by Vulkan.
        unsafe {
            for semaphore in self.present_ready.drain(..) {
                self.device.destroy_semaphore(semaphore, None);
            }
            destroy_slots(&self.device, self.command_pool, &mut self.slots);
        }
    }
}

unsafe fn destroy_slots(
    device: &ash::Device,
    command_pool: vk::CommandPool,
    slots: &mut Vec<CopySlot>,
) {
    for slot in slots.drain(..) {
        unsafe {
            device.destroy_fence(slot.fence, None);
        }
    }
    unsafe { device.destroy_command_pool(command_pool, None) };
}
