use ash::{Entry, vk};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use std::error::Error;
use winit::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
};

const FRAME_COUNT: u64 = 240;
const FRAMES_IN_FLIGHT: usize = 2;

struct VulkanApp {
    // The dynamically loaded Vulkan module must outlive every function pointer
    // stored in the instance and device wrappers.
    _entry: Entry,
    instance: ash::Instance,
    surface_loader: ash::khr::surface::Instance,
    surface: vk::SurfaceKHR,
    device: ash::Device,
    swapchain_loader: ash::khr::swapchain::Device,
    swapchain: vk::SwapchainKHR,
    images: Vec<vk::Image>,
    queue: vk::Queue,
    command_pool: vk::CommandPool,
    command_buffers: Vec<vk::CommandBuffer>,
    image_available: Vec<vk::Semaphore>,
    render_finished: Vec<vk::Semaphore>,
    fences: Vec<vk::Fence>,
    frame: u64,
}

impl VulkanApp {
    fn new(window: &winit::window::Window) -> Result<Self, Box<dyn Error>> {
        // SAFETY: Vulkan object lifetimes are owned by Self and destroyed in
        // reverse dependency order in Drop.
        unsafe {
            let entry = Entry::load()?;
            let display = window.display_handle()?.as_raw();
            let window_handle = window.window_handle()?.as_raw();
            let extension_names = ash_window::enumerate_required_extensions(display)?;
            let application = vk::ApplicationInfo::default()
                .application_name(c"snowglass-present-test")
                .api_version(vk::API_VERSION_1_3);
            let instance_info = vk::InstanceCreateInfo::default()
                .application_info(&application)
                .enabled_extension_names(extension_names);
            let instance = entry.create_instance(&instance_info, None)?;
            let surface =
                ash_window::create_surface(&entry, &instance, display, window_handle, None)?;
            let surface_loader = ash::khr::surface::Instance::new(&entry, &instance);

            let (physical_device, queue_family) = instance
                .enumerate_physical_devices()?
                .into_iter()
                .find_map(|physical_device| {
                    instance
                        .get_physical_device_queue_family_properties(physical_device)
                        .iter()
                        .enumerate()
                        .find_map(|(index, properties)| {
                            let graphics =
                                properties.queue_flags.contains(vk::QueueFlags::GRAPHICS);
                            let present = surface_loader
                                .get_physical_device_surface_support(
                                    physical_device,
                                    index as u32,
                                    surface,
                                )
                                .ok()?;
                            (graphics && present).then_some((physical_device, index as u32))
                        })
                })
                .ok_or("no graphics/present queue")?;

            let priorities = [1.0];
            let queue_info = vk::DeviceQueueCreateInfo::default()
                .queue_family_index(queue_family)
                .queue_priorities(&priorities);
            let queues = [queue_info];
            let device_extensions = [ash::khr::swapchain::NAME.as_ptr()];
            let device_info = vk::DeviceCreateInfo::default()
                .queue_create_infos(&queues)
                .enabled_extension_names(&device_extensions);
            let device = instance.create_device(physical_device, &device_info, None)?;
            let queue = device.get_device_queue(queue_family, 0);
            let swapchain_loader = ash::khr::swapchain::Device::new(&instance, &device);

            let capabilities = surface_loader
                .get_physical_device_surface_capabilities(physical_device, surface)?;
            let format = surface_loader
                .get_physical_device_surface_formats(physical_device, surface)?
                .into_iter()
                .find(|format| format.format == vk::Format::B8G8R8A8_UNORM)
                .unwrap_or(vk::SurfaceFormatKHR {
                    format: vk::Format::B8G8R8A8_UNORM,
                    color_space: vk::ColorSpaceKHR::SRGB_NONLINEAR,
                });
            let extent = if capabilities.current_extent.width != u32::MAX {
                capabilities.current_extent
            } else {
                vk::Extent2D {
                    width: 640,
                    height: 360,
                }
            };
            let image_count =
                (capabilities.min_image_count + 1).min(if capabilities.max_image_count == 0 {
                    u32::MAX
                } else {
                    capabilities.max_image_count
                });
            let swapchain_info = vk::SwapchainCreateInfoKHR::default()
                .surface(surface)
                .min_image_count(image_count)
                .image_format(format.format)
                .image_color_space(format.color_space)
                .image_extent(extent)
                .image_array_layers(1)
                .image_usage(vk::ImageUsageFlags::TRANSFER_DST)
                .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
                .pre_transform(capabilities.current_transform)
                .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
                .present_mode(vk::PresentModeKHR::FIFO)
                .clipped(true);
            let swapchain = swapchain_loader.create_swapchain(&swapchain_info, None)?;
            let images = swapchain_loader.get_swapchain_images(swapchain)?;
            let pool_info = vk::CommandPoolCreateInfo::default()
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
                .queue_family_index(queue_family);
            let command_pool = device.create_command_pool(&pool_info, None)?;
            let allocate_info = vk::CommandBufferAllocateInfo::default()
                .command_pool(command_pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(FRAMES_IN_FLIGHT as u32);
            let command_buffers = device.allocate_command_buffers(&allocate_info)?;

            let semaphore_info = vk::SemaphoreCreateInfo::default();
            let fence_info = vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED);
            let mut image_available = Vec::with_capacity(FRAMES_IN_FLIGHT);
            let mut render_finished = Vec::with_capacity(FRAMES_IN_FLIGHT);
            let mut fences = Vec::with_capacity(FRAMES_IN_FLIGHT);
            for _ in 0..FRAMES_IN_FLIGHT {
                image_available.push(device.create_semaphore(&semaphore_info, None)?);
                render_finished.push(device.create_semaphore(&semaphore_info, None)?);
                fences.push(device.create_fence(&fence_info, None)?);
            }

            Ok(Self {
                _entry: entry,
                instance,
                surface_loader,
                surface,
                device,
                swapchain_loader,
                swapchain,
                images,
                queue,
                command_pool,
                command_buffers,
                image_available,
                render_finished,
                fences,
                frame: 0,
            })
        }
    }

    fn draw(&mut self) -> Result<bool, vk::Result> {
        // SAFETY: synchronization ensures each per-frame command buffer and
        // semaphore is idle before reuse, and acquired image indices are valid.
        unsafe {
            let slot = self.frame as usize % FRAMES_IN_FLIGHT;
            self.device
                .wait_for_fences(&[self.fences[slot]], true, u64::MAX)?;
            let (image_index, _) = self.swapchain_loader.acquire_next_image(
                self.swapchain,
                u64::MAX,
                self.image_available[slot],
                vk::Fence::null(),
            )?;
            self.device.reset_fences(&[self.fences[slot]])?;
            let command = self.command_buffers[slot];
            self.device
                .reset_command_buffer(command, vk::CommandBufferResetFlags::empty())?;
            self.device
                .begin_command_buffer(command, &vk::CommandBufferBeginInfo::default())?;
            let to_clear = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .image(self.images[image_index as usize])
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });
            self.device.cmd_pipeline_barrier(
                command,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_clear],
            );
            let phase = (self.frame % 180) as f32 / 180.0;
            self.device.cmd_clear_color_image(
                command,
                self.images[image_index as usize],
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &vk::ClearColorValue {
                    float32: [phase, 0.2, 1.0 - phase, 1.0],
                },
                &[to_clear.subresource_range],
            );
            let to_present = vk::ImageMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::empty())
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
                .image(self.images[image_index as usize])
                .subresource_range(to_clear.subresource_range);
            self.device.cmd_pipeline_barrier(
                command,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[to_present],
            );
            self.device.end_command_buffer(command)?;
            let waits = [self.image_available[slot]];
            let stages = [vk::PipelineStageFlags::TRANSFER];
            let commands = [command];
            let signals = [self.render_finished[slot]];
            let submit = vk::SubmitInfo::default()
                .wait_semaphores(&waits)
                .wait_dst_stage_mask(&stages)
                .command_buffers(&commands)
                .signal_semaphores(&signals);
            self.device
                .queue_submit(self.queue, &[submit], self.fences[slot])?;
            let swapchains = [self.swapchain];
            let indices = [image_index];
            let present = vk::PresentInfoKHR::default()
                .wait_semaphores(&signals)
                .swapchains(&swapchains)
                .image_indices(&indices);
            self.swapchain_loader.queue_present(self.queue, &present)?;
            self.frame += 1;
            Ok(self.frame < FRAME_COUNT)
        }
    }
}

impl Drop for VulkanApp {
    fn drop(&mut self) {
        // SAFETY: all resources were created by these live parent objects and
        // the device is made idle before destruction.
        unsafe {
            let _ = self.device.device_wait_idle();
            for fence in self.fences.drain(..) {
                self.device.destroy_fence(fence, None);
            }
            for semaphore in self.render_finished.drain(..) {
                self.device.destroy_semaphore(semaphore, None);
            }
            for semaphore in self.image_available.drain(..) {
                self.device.destroy_semaphore(semaphore, None);
            }
            self.device.destroy_command_pool(self.command_pool, None);
            self.swapchain_loader
                .destroy_swapchain(self.swapchain, None);
            self.device.destroy_device(None);
            self.surface_loader.destroy_surface(self.surface, None);
            self.instance.destroy_instance(None);
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let window = WindowBuilder::new()
        .with_title("Snowglass Vulkan Present Test")
        .with_inner_size(LogicalSize::new(640, 360))
        .build(&event_loop)?;
    let mut app = VulkanApp::new(&window)?;
    event_loop.run(move |event, target| match event {
        Event::AboutToWait => window.request_redraw(),
        Event::WindowEvent {
            event: WindowEvent::RedrawRequested,
            ..
        } => match app.draw() {
            Ok(true) => {}
            Ok(false) => target.exit(),
            Err(error) => {
                eprintln!("present test failed: {error}");
                target.exit();
            }
        },
        Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } => target.exit(),
        _ => {}
    })?;
    Ok(())
}
