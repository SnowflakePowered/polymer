use super::PresentationSemaphores;
use ash::vk;
use windows::{
    Win32::{System::LibraryLoader::GetModuleHandleW, UI::WindowsAndMessaging::*},
    core::w,
};

#[test]
fn native_presentation_fence_releases_retired_stream_semaphores() {
    for (application_fence, detached_owner) in [(false, false), (true, false), (false, true)] {
        unsafe { WsiFixture::present(application_fence, detached_owner, 2) };
    }
}

struct WsiFixture;
impl WsiFixture {
    unsafe fn present(application_fence: bool, detached_owner: bool, swapchain_count: usize) {
        unsafe {
            // Opt-in loader regression: the app requests only ordinary WSI;
            // the loaded Polymer layer must negotiate presentation fences.
            let hook_negotiates = std::env::var_os("POLYMER_WSI_HOOK_NEGOTIATION").is_some();
            let graph_fixture = std::env::var_os("POLYMER_WSI_GRAPH").is_some();
            if graph_fixture {
                assert!(hook_negotiates && polymer_hook::config::tracking_enabled());
            }
            let hook_owns_fences = hook_negotiates && !application_fence;
            let mut viewer = std::env::var_os("POLYMER_WSI_VIEWER").map(|_| {
                assert!(
                    hook_negotiates,
                    "connected viewer requires the loader fixture"
                );
                polymer_test_viewer::WsiViewer::start(swapchain_count)
            });
            let entry = ash::Entry::load().unwrap();
            let extensions = [
                ash::khr::surface::NAME.as_ptr(),
                ash::khr::win32_surface::NAME.as_ptr(),
                ash::khr::get_surface_capabilities2::NAME.as_ptr(),
                ash::ext::surface_maintenance1::NAME.as_ptr(),
            ];
            let app = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_1);
            let instance = entry
                .create_instance(
                    &vk::InstanceCreateInfo::default()
                        .application_info(&app)
                        .enabled_extension_names(
                            &extensions[..if hook_negotiates { 2 } else { 4 }],
                        ),
                    None,
                )
                .unwrap();
            let module = GetModuleHandleW(None).unwrap();
            if hook_negotiates {
                GetModuleHandleW(w!("polymer_hook_vulkan_layer.dll"))
                    .expect("loader regression requires the Polymer layer DLL");
            }
            let windows = (0..swapchain_count)
                .map(|offset| {
                    CreateWindowExW(
                        WINDOW_EX_STYLE::default(),
                        w!("STATIC"),
                        w!("Polymer Vulkan fence fixture"),
                        WS_OVERLAPPEDWINDOW,
                        0,
                        0,
                        128 + offset as i32 * 32,
                        128,
                        None,
                        None,
                        Some(module.into()),
                        None,
                    )
                    .unwrap()
                })
                .collect::<Vec<_>>();
            let win32 = ash::khr::win32_surface::Instance::new(&entry, &instance);
            let surface_api = ash::khr::surface::Instance::new(&entry, &instance);
            let surfaces = windows
                .iter()
                .map(|window| {
                    win32
                        .create_win32_surface(
                            &vk::Win32SurfaceCreateInfoKHR::default()
                                .hinstance(module.0 as isize)
                                .hwnd(window.0 as isize),
                            None,
                        )
                        .unwrap()
                })
                .collect::<Vec<_>>();
            let physical = instance.enumerate_physical_devices().unwrap()[0];
            let family = instance
                .get_physical_device_queue_family_properties(physical)
                .iter()
                .enumerate()
                .find(|(index, family)| {
                    family.queue_flags.contains(vk::QueueFlags::GRAPHICS)
                        && surfaces.iter().all(|surface| {
                            surface_api
                                .get_physical_device_surface_support(
                                    physical,
                                    *index as u32,
                                    *surface,
                                )
                                .unwrap()
                        })
                })
                .map(|(index, _)| index as u32)
                .unwrap();
            let mut maintenance = vk::PhysicalDeviceSwapchainMaintenance1FeaturesEXT::default();
            instance.get_physical_device_features2(
                physical,
                &mut vk::PhysicalDeviceFeatures2::default().push_next(&mut maintenance),
            );
            assert_eq!(
                maintenance.swapchain_maintenance1,
                vk::TRUE,
                "native driver must support presentation fences"
            );
            let priorities = [1.0];
            let queues = [vk::DeviceQueueCreateInfo::default()
                .queue_family_index(family)
                .queue_priorities(&priorities)];
            let extensions = [
                ash::khr::swapchain::NAME.as_ptr(),
                ash::ext::swapchain_maintenance1::NAME.as_ptr(),
            ];
            let device_info = vk::DeviceCreateInfo::default()
                .queue_create_infos(&queues)
                .enabled_extension_names(&extensions[..if hook_negotiates { 1 } else { 2 }]);
            let device_info = if hook_negotiates {
                device_info
            } else {
                device_info.push_next(&mut maintenance)
            };
            let device = instance
                .create_device(physical, &device_info, None)
                .unwrap();
            let queue = device.get_device_queue(family, 0);
            let swapchain_api = ash::khr::swapchain::Device::new(&instance, &device);
            let swapchains = surfaces
                .iter()
                .map(|surface| {
                    let caps = surface_api
                        .get_physical_device_surface_capabilities(physical, *surface)
                        .unwrap();
                    let format = surface_api
                        .get_physical_device_surface_formats(physical, *surface)
                        .unwrap()[0];
                    swapchain_api
                        .create_swapchain(
                            &vk::SwapchainCreateInfoKHR::default()
                                .surface(*surface)
                                .min_image_count(caps.min_image_count)
                                .image_format(format.format)
                                .image_color_space(format.color_space)
                                .image_extent(caps.current_extent)
                                .image_array_layers(1)
                                .image_usage(
                                    vk::ImageUsageFlags::COLOR_ATTACHMENT
                                        | vk::ImageUsageFlags::TRANSFER_DST,
                                )
                                .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
                                .pre_transform(caps.current_transform)
                                .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
                                .present_mode(vk::PresentModeKHR::FIFO)
                                .clipped(true),
                            None,
                        )
                        .unwrap()
                })
                .collect::<Vec<_>>();
            let images = swapchains
                .iter()
                .map(|swapchain| swapchain_api.get_swapchain_images(*swapchain).unwrap())
                .collect::<Vec<_>>();
            let offscreen = if graph_fixture {
                let memory = instance.get_physical_device_memory_properties(physical);
                surfaces
                    .iter()
                    .enumerate()
                    .map(|(index, surface)| {
                        let format = surface_api
                            .get_physical_device_surface_formats(physical, *surface)
                            .unwrap()[0]
                            .format;
                        super::wsi_image::WsiImage::new(&device, &memory, format, index == 0)
                    })
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            let acquires = (0..swapchain_count)
                .map(|_| {
                    device
                        .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
                        .unwrap()
                })
                .collect::<Vec<_>>();
            let pool = PresentationSemaphores::default();
            let stream = pool.stream(&device, 10, images[0].len()).unwrap();
            let indices = swapchains
                .iter()
                .zip(&acquires)
                .map(|(swapchain, acquire)| {
                    swapchain_api
                        .acquire_next_image(*swapchain, 2_000_000_000, *acquire, vk::Fence::null())
                        .unwrap()
                        .0
                })
                .collect::<Vec<_>>();
            let commands = device
                .create_command_pool(
                    &vk::CommandPoolCreateInfo::default().queue_family_index(family),
                    None,
                )
                .unwrap();
            let command = device
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(commands)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(1),
                )
                .unwrap()[0];
            device
                .begin_command_buffer(command, &vk::CommandBufferBeginInfo::default())
                .unwrap();
            let mut barriers = images
                .iter()
                .zip(&indices)
                .map(|(images, index)| {
                    vk::ImageMemoryBarrier::default()
                        .old_layout(vk::ImageLayout::UNDEFINED)
                        .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                        .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .image(images[*index as usize])
                        .subresource_range(
                            vk::ImageSubresourceRange::default()
                                .aspect_mask(vk::ImageAspectFlags::COLOR)
                                .level_count(1)
                                .layer_count(1),
                        )
                })
                .collect::<Vec<_>>();
            device.cmd_pipeline_barrier(
                command,
                vk::PipelineStageFlags::TOP_OF_PIPE,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &barriers,
            );
            for (index, barrier) in barriers.iter_mut().enumerate() {
                let color = vk::ClearColorValue {
                    float32: if index == 0 {
                        [1.0, 0.0, 0.0, 1.0]
                    } else {
                        [0.0, 1.0, 0.0, 1.0]
                    },
                };
                if graph_fixture {
                    let extent = surface_api
                        .get_physical_device_surface_capabilities(physical, surfaces[index])
                        .unwrap()
                        .current_extent;
                    offscreen[index].blit(command, barrier.image, extent);
                } else {
                    device.cmd_clear_color_image(
                        command,
                        barrier.image,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        &color,
                        &[barrier.subresource_range],
                    );
                }
                barrier.old_layout = vk::ImageLayout::TRANSFER_DST_OPTIMAL;
                barrier.new_layout = vk::ImageLayout::PRESENT_SRC_KHR;
                barrier.src_access_mask = vk::AccessFlags::TRANSFER_WRITE;
                barrier.dst_access_mask = vk::AccessFlags::empty();
            }
            device.cmd_pipeline_barrier(
                command,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::BOTTOM_OF_PIPE,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &barriers,
            );
            device.end_command_buffer(command).unwrap();
            let waits = acquires;
            let stages = vec![vk::PipelineStageFlags::ALL_COMMANDS; swapchain_count];
            let signals = [stream[indices[0] as usize].raw()];
            let command_list = [command];
            device
                .queue_submit(
                    queue,
                    &[vk::SubmitInfo::default()
                        .wait_semaphores(&waits)
                        .wait_dst_stage_mask(&stages)
                        .command_buffers(&command_list)
                        .signal_semaphores(&signals)],
                    vk::Fence::null(),
                )
                .unwrap();
            let capture_state = crate::capture::CaptureState::default();
            let presents = swapchains
                .iter()
                .copied()
                .zip(indices.iter().copied())
                .collect::<Vec<_>>();
            let original = vk::PresentInfoKHR::default()
                .wait_semaphores(&waits)
                .swapchains(&swapchains)
                .image_indices(&indices);
            let app_fences = if application_fence {
                (0..swapchain_count)
                    .map(|_| {
                        device
                            .create_fence(&vk::FenceCreateInfo::default(), None)
                            .unwrap()
                    })
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            let null_fences = vec![vk::Fence::null(); swapchain_count];
            let mut null_info = vk::SwapchainPresentFenceInfoEXT::default().fences(&null_fences);
            assert!(matches!(
                crate::capture::PresentationCompletion::prepare(
                    &device,
                    &original.push_next(&mut null_info)
                ),
                Err(vk::Result::ERROR_FEATURE_NOT_PRESENT)
            ));
            let mut application_info =
                vk::SwapchainPresentFenceInfoEXT::default().fences(&app_fences);
            let original = if application_fence {
                original.push_next(&mut application_info)
            } else {
                original
            };
            // Inject a presentation enqueue allocation failure after the real GPU
            // submission has consumed the acquire semaphore. The retry must wait
            // on its existing replacement signal, without submitting another copy.
            let failed =
                crate::capture::PresentationCompletion::prepare(&device, &original).unwrap();
            assert!(
                failed
                    .submitted(vk::Result::ERROR_OUT_OF_HOST_MEMORY)
                    .unwrap()
                    .is_none()
            );
            capture_state.presentation_result(
                queue,
                &presents,
                &waits,
                Some(&signals),
                vk::Result::ERROR_OUT_OF_HOST_MEMORY,
            );
            let retry = capture_state
                .retry_presentation(queue, &presents, &waits)
                .unwrap();
            assert_eq!(retry.waits, signals);
            let present = vk::PresentInfoKHR::default()
                .wait_semaphores(&retry.waits)
                .swapchains(&swapchains)
                .image_indices(&indices);
            let present = if application_fence {
                present.push_next(&mut application_info)
            } else {
                present
            };
            let completion = (!hook_owns_fences).then(|| {
                crate::capture::PresentationCompletion::prepare(&device, &present).unwrap()
            });
            let mut fence_info = vk::SwapchainPresentFenceInfoEXT::default().fences(
                completion
                    .as_ref()
                    .map_or(&[], |completion| completion.fences()),
            );
            let present = if application_fence {
                assert!(
                    completion.as_ref().unwrap().fences().is_empty(),
                    "application fence must not be replaced"
                );
                present
            } else if hook_owns_fences {
                assert!(present.p_next.is_null());
                present
            } else {
                present.push_next(&mut fence_info)
            };
            let result = swapchain_api.queue_present(queue, &present).unwrap();
            if let Some(viewer) = &mut viewer {
                viewer.verify(swapchain_count);
            }
            if graph_fixture && std::env::var_os("POLYMER_WSI_OPAQUE").is_some() {
                let format = surface_api
                    .get_physical_device_surface_formats(physical, surfaces[0])
                    .unwrap()[0]
                    .format;
                super::ring_fixture::RingFixture::verify(
                    &instance,
                    physical,
                    &device,
                    queue,
                    family,
                    offscreen[0].image,
                    format,
                );
            }
            capture_state.presentation_result(
                queue,
                &presents,
                &waits,
                Some(&retry.waits),
                vk::Result::SUCCESS,
            );
            assert!(
                capture_state
                    .retry_presentation(queue, &presents, &waits)
                    .is_none()
            );
            let completion = completion.and_then(|completion| {
                completion
                    .submitted(if result {
                        vk::Result::SUBOPTIMAL_KHR
                    } else {
                        vk::Result::SUCCESS
                    })
                    .unwrap()
            });
            if let Some(completion) = &completion {
                if detached_owner {
                    completion.retain_signals(pool.retain_signals(&signals));
                } else {
                    pool.record_completion(completion.clone());
                }
            }
            drop(stream);
            if hook_owns_fences {
                // No fixture-owned fence or WaitIdle is available. The layer's
                // destroy prefix must wait for its own presentation fences
                // before these application-owned wait semaphores are released.
                for swapchain in &swapchains {
                    swapchain_api.destroy_swapchain(*swapchain, None);
                }
            }
            // Production pool retirement must wait for the actual presentation
            // fence without relying on a caller-supplied WaitIdle boundary.
            drop(pool);
            if let Some(completion) = &completion {
                if detached_owner {
                    // The old pool is gone. Only the completion object retains
                    // the native signal while the presentation is in flight.
                    assert!(completion.wait());
                }
                assert!(completion.complete());
            }
            drop(completion);
            drop(retry);
            if application_fence {
                assert!(device.get_fence_status(app_fences[0]).unwrap());
                device
                    .wait_for_fences(&app_fences, true, 2_000_000_000)
                    .unwrap();
                // The borrowed guard must leave the application's fence alive and
                // resettable after waiting for the actual presentation operation.
                device.reset_fences(&app_fences).unwrap();
                assert!(!device.get_fence_status(app_fences[0]).unwrap());
                for fence in app_fences {
                    device.destroy_fence(fence, None);
                }
            }
            for acquire in waits {
                device.destroy_semaphore(acquire, None);
            }
            device.destroy_command_pool(commands, None);
            if !hook_owns_fences {
                for swapchain in swapchains {
                    swapchain_api.destroy_swapchain(swapchain, None);
                }
            }
            drop(offscreen);
            device.destroy_device(None);
            for surface in surfaces {
                surface_api.destroy_surface(surface, None);
            }
            for window in windows {
                DestroyWindow(window).unwrap();
            }
            instance.destroy_instance(None);
        }
    }
}
