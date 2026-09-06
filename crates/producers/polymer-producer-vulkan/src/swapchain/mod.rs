mod presentation;
mod slots;
use crate::{ExportImageRing, ExportTimelineSemaphore, ExternalHandleMode};
use ash::{vk, vk::Handle};
pub(crate) use presentation::PresentationSemaphore;
pub(crate) use presentation::PresentationSemaphores;
use slots::SlotState;
use std::ffi::c_void;
use std::sync::Arc;

pub type SetDeviceLoaderData = unsafe extern "system" fn(vk::Device, *mut c_void) -> vk::Result;

pub struct CopySubmission {
    pub present_ready: vk::Semaphore,
    pub slot: u32,
    pub sync_value: u64,
}

struct CopySlot {
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
    state: SlotState,
}

/// Records and submits swapchain-to-export-ring copies on an application's
/// presentation queue.
pub struct SwapchainCopyEngine {
    device: ash::Device,
    ring: ExportImageRing,
    producer_timeline: ExportTimelineSemaphore,
    consumer_timeline: Option<ExportTimelineSemaphore>,
    source_images: Vec<vk::Image>,
    extent: vk::Extent2D,
    command_pool: vk::CommandPool,
    slots: Vec<CopySlot>,
    present_ready: Vec<Arc<PresentationSemaphore>>,
    scheduler: polymer_presentation::scheduling::SlotScheduler,
    next_sync_value: u64,
    queue_family_index: u32,
}

impl SwapchainCopyEngine {
    pub(crate) fn create(
        device: ash::Device,
        presentation_semaphores: &PresentationSemaphores,
        stream: u64,
        ring: ExportImageRing,
        producer_timeline: ExportTimelineSemaphore,
        consumer_timeline: Option<ExportTimelineSemaphore>,
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
                    state: SlotState::default(),
                });
            }
            let present_ready =
                match presentation_semaphores.stream(&device, stream, source_images.len()) {
                    Ok(semaphores) => semaphores,
                    Err(error) => {
                        destroy_slots(&device, command_pool, &mut slots);
                        return Err(error);
                    }
                };
            Ok(Self {
                device,
                ring,
                producer_timeline,
                consumer_timeline,
                source_images,
                extent,
                command_pool,
                slots,
                present_ready,
                scheduler: Default::default(),
                next_sync_value: 1,
                queue_family_index,
            })
        }
    }

    /// The exported ring and timelines this engine copies into, for
    /// re-publishing the stream over a fresh viewer connection.
    pub fn transport_parts(
        &self,
    ) -> (
        &ExportImageRing,
        &ExportTimelineSemaphore,
        Option<&ExportTimelineSemaphore>,
    ) {
        (
            &self.ring,
            &self.producer_timeline,
            self.consumer_timeline.as_ref(),
        )
    }

    /// Waits for every in-flight copy to retire. Required before dropping the
    /// engine while the device is still in use, since dropping destroys the
    /// command pool the copies were recorded from.
    pub fn wait_idle(&self) {
        let fences = self
            .slots
            .iter()
            .filter(|slot| slot.state.pending())
            .map(|slot| slot.fence)
            .collect::<Vec<_>>();
        if fences.is_empty() {
            return;
        }
        unsafe {
            let _ = self.device.wait_for_fences(&fences, true, u64::MAX);
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
        self.submit_image_copy_inner(
            queue,
            source_image_index,
            source_image,
            self.extent,
            vk::ImageLayout::PRESENT_SRC_KHR,
            application_waits,
            true,
            vk::Offset2D::default(),
            (0, 0),
        )
    }

    /// Copies an identified presentation source into a top-left, aspect-preserving
    /// region of the export image.
    pub fn submit_image_copy(
        &mut self,
        queue: vk::Queue,
        source_image_index: u32,
        source_image: vk::Image,
        source_subresource: (u32, u32),
        source_offset: vk::Offset2D,
        source_extent: vk::Extent2D,
        source_layout: vk::ImageLayout,
        application_waits: &[vk::Semaphore],
    ) -> Result<CopySubmission, vk::Result> {
        self.submit_image_copy_inner(
            queue,
            source_image_index,
            source_image,
            source_extent,
            source_layout,
            application_waits,
            true,
            source_offset,
            source_subresource,
        )
    }

    pub fn submit_region_copy(
        &mut self,
        queue: vk::Queue,
        source_image_index: u32,
        source_offset: vk::Offset2D,
        source_extent: vk::Extent2D,
        application_waits: &[vk::Semaphore],
    ) -> Result<CopySubmission, vk::Result> {
        let Some(&source_image) = self.source_images.get(source_image_index as usize) else {
            return Err(vk::Result::ERROR_OUT_OF_DATE_KHR);
        };
        self.submit_image_copy_inner(
            queue,
            source_image_index,
            source_image,
            source_extent,
            vk::ImageLayout::PRESENT_SRC_KHR,
            application_waits,
            true,
            source_offset,
            (0, 0),
        )
    }

    #[expect(clippy::too_many_arguments)]
    fn submit_image_copy_inner(
        &mut self,
        queue: vk::Queue,
        source_image_index: u32,
        source_image: vk::Image,
        source_extent: vk::Extent2D,
        source_layout: vk::ImageLayout,
        application_waits: &[vk::Semaphore],
        signal_present: bool,
        source_offset: vk::Offset2D,
        source_subresource: (u32, u32),
    ) -> Result<CopySubmission, vk::Result> {
        if source_extent.width == 0 || source_extent.height == 0 {
            return Err(vk::Result::ERROR_INITIALIZATION_FAILED);
        }
        let ready = signal_present
            .then(|| {
                self.present_ready
                    .get(source_image_index as usize)
                    .map(|semaphore| semaphore.raw())
            })
            .flatten()
            .unwrap_or(vk::Semaphore::null());
        let consumer_completed = self
            .consumer_timeline
            .as_ref()
            .map(|timeline| unsafe {
                self.device
                    .get_semaphore_counter_value(timeline.semaphore())
            })
            .transpose()?;
        let sync_value = self.next_sync_value;
        let reservation = self
            .scheduler
            .try_acquire(self.slots.len(), |index| {
                let slot = &self.slots[index];
                let producer_completed = !slot.state.pending()
                    || unsafe { self.device.get_fence_status(slot.fence) }.unwrap_or(false);
                slot.state
                    .try_acquire(producer_completed, consumer_completed)
            })
            .ok_or(vk::Result::NOT_READY)?;
        let slot_index = reservation.0;
        let slot = &self.slots[slot_index];
        let destination_image = self.ring.slots()[slot_index].image();
        let d3d12_handoff = self.ring.desc().handle_mode == ExternalHandleMode::D3d12;

        // SAFETY: the slot fence protects command-buffer reuse; swapchain image
        // ownership remains on this queue and the caller supplies valid waits.
        unsafe {
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
                .old_layout(source_layout)
                .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .image(source_image)
                .subresource_range(vk::ImageSubresourceRange {
                    base_mip_level: source_subresource.0,
                    base_array_layer: source_subresource.1,
                    ..color_range
                });
            let destination_to_transfer = vk::ImageMemoryBarrier::default()
                .src_access_mask(if slot.state.initialized() {
                    vk::AccessFlags::MEMORY_READ
                } else {
                    vk::AccessFlags::empty()
                })
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .old_layout(if slot.state.initialized() || d3d12_handoff {
                    vk::ImageLayout::GENERAL
                } else {
                    vk::ImageLayout::UNDEFINED
                })
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_queue_family_index(if d3d12_handoff {
                    vk::QUEUE_FAMILY_FOREIGN_EXT
                } else {
                    vk::QUEUE_FAMILY_IGNORED
                })
                .dst_queue_family_index(if d3d12_handoff {
                    self.queue_family_index
                } else {
                    vk::QUEUE_FAMILY_IGNORED
                })
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
            let tracked_source = source_layout != vk::ImageLayout::PRESENT_SRC_KHR;
            if tracked_source {
                self.device.cmd_clear_color_image(
                    slot.command_buffer,
                    destination_image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &vk::ClearColorValue {
                        float32: [1.0, 0.0, 1.0, 1.0],
                    },
                    &[color_range],
                );
            }
            let destination_extent = fit_inside(source_extent, self.extent);
            let subresource = vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            };
            let blit_region = vk::ImageBlit::default()
                .src_subresource(vk::ImageSubresourceLayers {
                    mip_level: source_subresource.0,
                    base_array_layer: source_subresource.1,
                    ..subresource
                })
                .dst_subresource(subresource)
                .src_offsets([
                    vk::Offset3D {
                        x: source_offset.x,
                        y: source_offset.y,
                        z: 0,
                    },
                    vk::Offset3D {
                        x: source_offset.x + source_extent.width as i32,
                        y: source_offset.y + source_extent.height as i32,
                        z: 1,
                    },
                ])
                .dst_offsets([
                    vk::Offset3D::default(),
                    vk::Offset3D {
                        x: destination_extent.width as i32,
                        y: destination_extent.height as i32,
                        z: 1,
                    },
                ]);
            if tracked_source {
                self.device.cmd_blit_image(
                    slot.command_buffer,
                    source_image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    destination_image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[blit_region],
                    vk::Filter::LINEAR,
                );
            } else {
                self.device.cmd_copy_image(
                    slot.command_buffer,
                    source_image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    destination_image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[vk::ImageCopy::default()
                        .src_subresource(blit_region.src_subresource)
                        .src_offset(blit_region.src_offsets[0])
                        .dst_subresource(subresource)
                        .extent(vk::Extent3D {
                            width: source_extent.width.min(self.extent.width),
                            height: source_extent.height.min(self.extent.height),
                            depth: 1,
                        })],
                );
            }
            let source_to_present = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::TRANSFER_READ)
                .dst_access_mask(vk::AccessFlags::MEMORY_READ)
                .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .new_layout(source_layout)
                .image(source_image)
                .subresource_range(vk::ImageSubresourceRange {
                    base_mip_level: source_subresource.0,
                    base_array_layer: source_subresource.1,
                    ..color_range
                });
            let destination_to_general = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::MEMORY_READ)
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(if d3d12_handoff {
                    self.queue_family_index
                } else {
                    vk::QUEUE_FAMILY_IGNORED
                })
                .dst_queue_family_index(if d3d12_handoff {
                    vk::QUEUE_FAMILY_FOREIGN_EXT
                } else {
                    vk::QUEUE_FAMILY_IGNORED
                })
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

            let waits = application_waits.to_vec();
            let wait_values = vec![0; waits.len()];
            let wait_stages = vec![vk::PipelineStageFlags::TRANSFER; waits.len()];
            let command_buffers = [slot.command_buffer];
            let mut signals = vec![self.producer_timeline.semaphore()];
            let mut signal_values = vec![sync_value];
            if signal_present {
                signals.insert(0, ready);
                signal_values.insert(0, 0);
            }
            let mut timeline_submit = vk::TimelineSemaphoreSubmitInfo::default()
                .wait_semaphore_values(&wait_values)
                .signal_semaphore_values(&signal_values);
            let submit = vk::SubmitInfo::default()
                .push_next(&mut timeline_submit)
                .wait_semaphores(&waits)
                .wait_dst_stage_mask(&wait_stages)
                .command_buffers(&command_buffers)
                .signal_semaphores(&signals);
            self.device.reset_fences(&[slot.fence])?;
            slot.state
                .submitted(self.device.queue_submit(queue, &[submit], slot.fence))?;
            self.slots[slot_index].state.published(sync_value);
            self.scheduler.commit(slot_index, self.slots.len());
            self.next_sync_value += 1;
            Ok(CopySubmission {
                present_ready: ready,
                slot: slot_index as u32,
                sync_value,
            })
        }
    }
}

pub fn fit_inside(source: vk::Extent2D, bounds: vk::Extent2D) -> vk::Extent2D {
    if source.width <= bounds.width && source.height <= bounds.height {
        return source;
    }
    let scale = (bounds.width as f64 / source.width as f64)
        .min(bounds.height as f64 / source.height as f64);
    vk::Extent2D {
        width: (source.width as f64 * scale).floor().max(1.0) as u32,
        height: (source.height as f64 * scale).floor().max(1.0) as u32,
    }
}

impl Drop for SwapchainCopyEngine {
    fn drop(&mut self) {
        // The layer calls device_wait_idle before removing engines at device
        // destruction; swapchain destruction is externally synchronized by Vulkan.
        unsafe {
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

impl SwapchainCopyEngine {
    pub(crate) fn ready_to_retire(&mut self) -> bool {
        self.slots.iter().all(|slot| {
            !slot.state.pending()
                || unsafe { self.device.get_fence_status(slot.fence) }.unwrap_or(false)
        })
    }
}
