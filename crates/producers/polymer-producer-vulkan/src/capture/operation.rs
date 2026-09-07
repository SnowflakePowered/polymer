use super::{StreamResources, tracking};
use ash::vk;

impl StreamResources {
    pub(super) unsafe fn copy_frame(
        &mut self,
        queue: vk::Queue,
        image_index: u32,
        source: Option<&tracking::PresentSource>,
        capture_rect: Option<vk::Rect2D>,
        rect_is_content: bool,
        waits: &[vk::Semaphore],
    ) -> Result<Option<crate::swapchain::CopySubmission>, vk::Result> {
        let engine = self.copy_engine.as_mut().expect("active copy engine");
        let submission = if let Some(source) = source {
            engine.submit_image_copy(
                queue,
                image_index,
                source.image,
                source.subresource,
                vk::Offset2D {
                    x: source.selection.source_rect.x,
                    y: source.selection.source_rect.y,
                },
                vk::Extent2D {
                    width: source.selection.source_rect.extent.width,
                    height: source.selection.source_rect.extent.height,
                },
                source.layout,
                waits,
            )
        } else if rect_is_content {
            let Some(rect) = capture_rect else {
                return Ok(None);
            };
            engine.submit_region_copy(queue, image_index, rect.offset, rect.extent, waits)
        } else {
            engine.submit_copy(queue, image_index, waits)
        };
        match submission {
            Ok(submission) => Ok(Some(submission)),
            Err(vk::Result::NOT_READY) => Ok(None),
            Err(error) => Err(error),
        }
    }
}
