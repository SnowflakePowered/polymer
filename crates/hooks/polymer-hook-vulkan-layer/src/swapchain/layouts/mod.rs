use super::*;
mod barriers;
mod description;
mod render_pass;
use std::collections::BTreeMap;

#[derive(Default)]
pub(crate) struct Layouts {
    views: DashMap<vk::ImageView, bool>,
    framebuffers: DashMap<vk::Framebuffer, Vec<bool>>,
    passes: DashMap<vk::RenderPass, Pass>,
}
struct Pass {
    attachments: Vec<usize>,
    variants: BTreeMap<usize, vk::RenderPass>,
}
impl Layouts {
    pub(super) fn destroy(&self, device: &ash::Device) {
        for pass in self.passes.iter() {
            for &variant in pass.variants.values() {
                unsafe {
                    device.destroy_render_pass(variant, None);
                }
            }
        }
        self.passes.clear();
    }
    fn layout(layout: vk::ImageLayout) -> vk::ImageLayout {
        if layout == vk::ImageLayout::PRESENT_SRC_KHR {
            vk::ImageLayout::GENERAL
        } else {
            layout
        }
    }
    unsafe fn slice<'a, T>(pointer: *const T, count: u32) -> &'a [T] {
        if count == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(pointer, count as usize) }
        }
    }
    fn virtual_views(&self, views: &[vk::ImageView]) -> Vec<bool> {
        views
            .iter()
            .map(|view| self.views.get(view).is_some_and(|v| *v))
            .collect()
    }
    unsafe fn begin(&self, info: &vk::RenderPassBeginInfo<'_>) -> vk::RenderPass {
        let Some(pass) = self.passes.get(&info.render_pass) else {
            return info.render_pass;
        };
        let mut views = self
            .framebuffers
            .get(&info.framebuffer)
            .map(|v| v.clone())
            .unwrap_or_default();
        let mut current = info.p_next.cast::<vk::BaseInStructure<'_>>();
        while let Some(base) = unsafe { current.as_ref() } {
            if base.s_type == vk::StructureType::RENDER_PASS_ATTACHMENT_BEGIN_INFO {
                let attachments =
                    unsafe { &*current.cast::<vk::RenderPassAttachmentBeginInfo<'_>>() };
                views = self.virtual_views(unsafe {
                    Self::slice(attachments.p_attachments, attachments.attachment_count)
                });
            }
            current = base.p_next;
        }
        let mask = pass
            .attachments
            .iter()
            .enumerate()
            .fold(0, |mask, (bit, &index)| {
                mask | (usize::from(views.get(index).copied().unwrap_or(false)) << bit)
            });
        pass.variants
            .get(&mask)
            .copied()
            .unwrap_or(info.render_pass)
    }
}
pub(super) fn hook(name: &[u8]) -> vk::PFN_vkVoidFunction {
    barriers::hook(name).or_else(|| render_pass::hook(name))
}
