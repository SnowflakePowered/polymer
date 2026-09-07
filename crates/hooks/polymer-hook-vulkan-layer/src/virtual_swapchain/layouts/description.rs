use super::*;

pub(super) trait Attachment: Copy {
    fn presents(&self) -> bool;
    fn translate(&mut self);
}
macro_rules! attachment {
    ($ty:ty) => {
        impl Attachment for $ty {
            fn presents(&self) -> bool {
                self.initial_layout == vk::ImageLayout::PRESENT_SRC_KHR
                    || self.final_layout == vk::ImageLayout::PRESENT_SRC_KHR
            }
            fn translate(&mut self) {
                self.initial_layout = Layouts::layout(self.initial_layout);
                self.final_layout = Layouts::layout(self.final_layout);
            }
        }
    };
}
attachment!(vk::AttachmentDescription);
attachment!(vk::AttachmentDescription2<'_>);

pub(super) trait Description: Copy {
    type Attachment: Attachment;
    type Create: Copy;
    unsafe fn next(gdpa: vk::PFN_vkGetDeviceProcAddr, device: vk::Device) -> Option<Self::Create>;
    unsafe fn attachments(&self) -> &[Self::Attachment];
    unsafe fn create(
        &self,
        next: Self::Create,
        device: vk::Device,
        attachments: &[Self::Attachment],
        allocator: *const vk::AllocationCallbacks<'_>,
        output: *mut vk::RenderPass,
    ) -> vk::Result;
}
macro_rules! description {
    ($kind:ident, $attachment:ty, $create:ty, $core:literal, $alias:literal) => {
        impl<'a> Description for vk::$kind<'a> {
            type Attachment = $attachment;
            type Create = $create;
            unsafe fn next(
                gdpa: vk::PFN_vkGetDeviceProcAddr,
                device: vk::Device,
            ) -> Option<Self::Create> {
                unsafe {
                    crate::terminal::device_proc(gdpa, device, $core)
                        .or_else(|| crate::terminal::device_proc(gdpa, device, $alias))
                }
            }
            unsafe fn attachments(&self) -> &[Self::Attachment] {
                unsafe { Layouts::slice(self.p_attachments, self.attachment_count) }
            }
            unsafe fn create(
                &self,
                next: Self::Create,
                device: vk::Device,
                attachments: &[Self::Attachment],
                allocator: *const vk::AllocationCallbacks<'_>,
                output: *mut vk::RenderPass,
            ) -> vk::Result {
                let info = Self {
                    p_attachments: attachments.as_ptr(),
                    ..*self
                };
                unsafe { next(device, &info, allocator, output) }
            }
        }
    };
}
description!(
    RenderPassCreateInfo,
    vk::AttachmentDescription,
    vk::PFN_vkCreateRenderPass,
    c"vkCreateRenderPass",
    c"vkCreateRenderPass"
);
description!(
    RenderPassCreateInfo2,
    vk::AttachmentDescription2<'a>,
    vk::PFN_vkCreateRenderPass2,
    c"vkCreateRenderPass2",
    c"vkCreateRenderPass2KHR"
);

impl Pass {
    pub(super) unsafe fn create_device<D: Description>(
        device: vk::Device,
        info: &D,
        allocator: *const vk::AllocationCallbacks<'_>,
        output: *mut vk::RenderPass,
    ) -> vk::Result {
        let Some(gdpa) = crate::layer::device_gdpa(device) else {
            return vk::Result::ERROR_DEVICE_LOST;
        };
        let Some(next) = (unsafe { D::next(gdpa, device) }) else {
            return vk::Result::ERROR_DEVICE_LOST;
        };
        let attachments = unsafe { info.attachments() };
        let Ok(owner) = Device::get(device) else {
            return unsafe { info.create(next, device, attachments, allocator, output) };
        };
        let indices = attachments
            .iter()
            .enumerate()
            .filter_map(|(index, item)| item.presents().then_some(index))
            .collect::<Vec<_>>();
        let pass = Self::create(indices.clone(), &owner.capture.device, |mask| {
            let mut attachments = attachments.to_vec();
            for (bit, &index) in indices.iter().enumerate() {
                if mask & (1 << bit) != 0 {
                    attachments[index].translate();
                }
            }
            let mut handle = vk::RenderPass::null();
            let result =
                unsafe { info.create(next, device, &attachments, std::ptr::null(), &mut handle) };
            if result == vk::Result::SUCCESS {
                Ok(handle)
            } else {
                Err(result)
            }
        });
        let pass = match pass {
            Ok(pass) => pass,
            Err(error) => return error,
        };
        let result = unsafe { info.create(next, device, attachments, allocator, output) };
        if result == vk::Result::SUCCESS {
            owner.layouts.passes.insert(unsafe { *output }, pass);
        } else {
            for handle in pass.variants.values() {
                unsafe {
                    owner.capture.device.destroy_render_pass(*handle, None);
                }
            }
        }
        result
    }
}
