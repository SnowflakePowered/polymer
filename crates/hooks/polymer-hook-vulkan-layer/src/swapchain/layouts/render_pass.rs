use super::*;

// Variants are created while the application's complete creation description is
// borrowed. Only native resource handles survive this call, never guest commands
// or pointers into guest memory. Layouts do not affect render-pass compatibility.
impl Pass {
    pub(super) fn create(
        attachments: Vec<usize>,
        device: &ash::Device,
        mut create: impl FnMut(usize) -> Result<vk::RenderPass, vk::Result>,
    ) -> Result<Self, vk::Result> {
        // Bound the number of native objects allocated by one application call.
        if attachments.len() > 8 {
            return Err(vk::Result::ERROR_OUT_OF_HOST_MEMORY);
        }
        let mut variants = BTreeMap::new();
        for mask in 1..(1usize << attachments.len()) {
            match create(mask) {
                Ok(handle) => {
                    variants.insert(mask, handle);
                }
                Err(error) => {
                    for handle in variants.values() {
                        unsafe {
                            device.destroy_render_pass(*handle, None);
                        }
                    }
                    return Err(error);
                }
            }
        }
        Ok(Self {
            attachments,
            variants,
        })
    }
}
unsafe extern "system" fn create(
    device: vk::Device,
    info: *const vk::RenderPassCreateInfo<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    output: *mut vk::RenderPass,
) -> vk::Result {
    unsafe { Pass::create_device(device, &*info, allocator, output) }
}
unsafe extern "system" fn create2(
    device: vk::Device,
    info: *const vk::RenderPassCreateInfo2<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    output: *mut vk::RenderPass,
) -> vk::Result {
    unsafe { Pass::create_device(device, &*info, allocator, output) }
}
unsafe extern "system" fn destroy(
    device: vk::Device,
    pass: vk::RenderPass,
    allocator: *const vk::AllocationCallbacks<'_>,
) {
    if let Ok(owner) = Device::get(device) {
        if let Some((_, pass)) = owner.layouts.passes.remove(&pass) {
            for handle in pass.variants.values() {
                unsafe {
                    owner.external.device.destroy_render_pass(*handle, None);
                }
            }
        }
    }
    if let Some(gdpa) = crate::layer::device_gdpa(device) {
        if let Some(next) = unsafe {
            crate::terminal::device_proc::<vk::PFN_vkDestroyRenderPass>(
                gdpa,
                device,
                c"vkDestroyRenderPass",
            )
        } {
            unsafe {
                next(device, pass, allocator);
            }
        }
    }
}
unsafe extern "system" fn begin(
    command: vk::CommandBuffer,
    info: *const vk::RenderPassBeginInfo<'_>,
    contents: vk::SubpassContents,
) {
    let Some((gdpa, device)) = crate::layer::command_dispatch_data(command) else {
        return;
    };
    let Some(next): Option<vk::PFN_vkCmdBeginRenderPass> =
        (unsafe { crate::terminal::device_proc(gdpa, device, c"vkCmdBeginRenderPass") })
    else {
        return;
    };
    let mut translated = unsafe { *info };
    if let Ok(owner) = Device::get(device) {
        translated.render_pass = unsafe { owner.layouts.begin(&translated) };
    }
    unsafe {
        next(command, &translated, contents);
    }
}
unsafe extern "system" fn begin2(
    command: vk::CommandBuffer,
    info: *const vk::RenderPassBeginInfo<'_>,
    subpass: *const vk::SubpassBeginInfo<'_>,
) {
    let Some((gdpa, device)) = crate::layer::command_dispatch_data(command) else {
        return;
    };
    let Some(next): Option<vk::PFN_vkCmdBeginRenderPass2> = (unsafe {
        crate::terminal::device_proc(gdpa, device, c"vkCmdBeginRenderPass2")
            .or_else(|| crate::terminal::device_proc(gdpa, device, c"vkCmdBeginRenderPass2KHR"))
    }) else {
        return;
    };
    let mut translated = unsafe { *info };
    if let Ok(owner) = Device::get(device) {
        translated.render_pass = unsafe { owner.layouts.begin(&translated) };
    }
    unsafe {
        next(command, &translated, subpass);
    }
}
unsafe extern "system" fn view(
    device: vk::Device,
    info: *const vk::ImageViewCreateInfo<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    output: *mut vk::ImageView,
) -> vk::Result {
    let Some(gdpa) = crate::layer::device_gdpa(device) else {
        return vk::Result::ERROR_DEVICE_LOST;
    };
    let Some(next): Option<vk::PFN_vkCreateImageView> =
        (unsafe { crate::terminal::device_proc(gdpa, device, c"vkCreateImageView") })
    else {
        return vk::Result::ERROR_DEVICE_LOST;
    };
    let result = unsafe { next(device, info, allocator, output) };
    if result == vk::Result::SUCCESS {
        if let Ok(owner) = Device::get(device) {
            owner.layouts.views.insert(
                unsafe { *output },
                owner.images.contains_key(&unsafe { (*info).image }),
            );
        }
    }
    result
}
unsafe extern "system" fn destroy_view(
    device: vk::Device,
    view: vk::ImageView,
    allocator: *const vk::AllocationCallbacks<'_>,
) {
    if let Ok(owner) = Device::get(device) {
        owner.layouts.views.remove(&view);
    }
    if let Some(gdpa) = crate::layer::device_gdpa(device) {
        if let Some(next) = unsafe {
            crate::terminal::device_proc::<vk::PFN_vkDestroyImageView>(
                gdpa,
                device,
                c"vkDestroyImageView",
            )
        } {
            unsafe {
                next(device, view, allocator);
            }
        }
    }
}
unsafe extern "system" fn framebuffer(
    device: vk::Device,
    info: *const vk::FramebufferCreateInfo<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    output: *mut vk::Framebuffer,
) -> vk::Result {
    let Some(gdpa) = crate::layer::device_gdpa(device) else {
        return vk::Result::ERROR_DEVICE_LOST;
    };
    let Some(next): Option<vk::PFN_vkCreateFramebuffer> =
        (unsafe { crate::terminal::device_proc(gdpa, device, c"vkCreateFramebuffer") })
    else {
        return vk::Result::ERROR_DEVICE_LOST;
    };
    let result = unsafe { next(device, info, allocator, output) };
    if result == vk::Result::SUCCESS {
        if let Ok(owner) = Device::get(device) {
            let info = unsafe { &*info };
            let views = if info.flags.contains(vk::FramebufferCreateFlags::IMAGELESS) {
                Vec::new()
            } else {
                owner.layouts.virtual_views(unsafe {
                    Layouts::slice(info.p_attachments, info.attachment_count)
                })
            };
            owner.layouts.framebuffers.insert(unsafe { *output }, views);
        }
    }
    result
}
unsafe extern "system" fn destroy_framebuffer(
    device: vk::Device,
    framebuffer: vk::Framebuffer,
    allocator: *const vk::AllocationCallbacks<'_>,
) {
    if let Ok(owner) = Device::get(device) {
        owner.layouts.framebuffers.remove(&framebuffer);
    }
    if let Some(gdpa) = crate::layer::device_gdpa(device) {
        if let Some(next) = unsafe {
            crate::terminal::device_proc::<vk::PFN_vkDestroyFramebuffer>(
                gdpa,
                device,
                c"vkDestroyFramebuffer",
            )
        } {
            unsafe {
                next(device, framebuffer, allocator);
            }
        }
    }
}
pub(super) fn hook(name: &[u8]) -> vk::PFN_vkVoidFunction {
    crate::hook(match name {
        b"vkCreateRenderPass" => create as *const (),
        b"vkCreateRenderPass2" | b"vkCreateRenderPass2KHR" => create2 as *const (),
        b"vkDestroyRenderPass" => destroy as *const (),
        b"vkCmdBeginRenderPass" => begin as *const (),
        b"vkCmdBeginRenderPass2" | b"vkCmdBeginRenderPass2KHR" => begin2 as *const (),
        b"vkCreateImageView" => view as *const (),
        b"vkDestroyImageView" => destroy_view as *const (),
        b"vkCreateFramebuffer" => framebuffer as *const (),
        b"vkDestroyFramebuffer" => destroy_framebuffer as *const (),
        _ => return None,
    })
}
