use super::{StreamResources, tracking};
use ash::vk;
use polymer_presentation::producer::CaptureOperation;

pub(super) struct CopyFrame<'a> {
    pub queue: vk::Queue,
    pub image_index: u32,
    pub source: Option<&'a tracking::PresentSource>,
    pub capture_rect: Option<vk::Rect2D>,
    pub rect_is_content: bool,
    pub waits: &'a [vk::Semaphore],
}
impl CaptureOperation for CopyFrame<'_> {
    type Backend = StreamResources;
    type Output = crate::swapchain::CopySubmission;
    type Error = vk::Result;
    unsafe fn submit(
        self,
        backend: &mut StreamResources,
    ) -> Result<Option<Self::Output>, vk::Result> {
        let engine = backend.copy_engine.as_mut().expect("active copy engine");
        let submission = if let Some(source) = self.source {
            engine.submit_image_copy(
                self.queue,
                self.image_index,
                source.image,
                source.subresource,
                source.source_offset,
                source.extent,
                source.layout,
                self.waits,
            )
        } else if self.rect_is_content {
            let Some(rect) = self.capture_rect else {
                return Ok(None);
            };
            engine.submit_region_copy(
                self.queue,
                self.image_index,
                rect.offset,
                rect.extent,
                self.waits,
            )
        } else {
            engine.submit_copy(self.queue, self.image_index, self.waits)
        };
        match submission {
            Ok(submission) => Ok(Some(submission)),
            Err(vk::Result::NOT_READY) => Ok(None),
            Err(error) => Err(error),
        }
    }
}
