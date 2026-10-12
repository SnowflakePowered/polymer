use super::*;
use crate::shell::protocol::VideoRequest;
use polymer_composition::VideoEvent;

impl GameHost {
    pub fn handle_video(&mut self, request: VideoRequest) {
        match request {
            VideoRequest::Open { id, bytes, sink } => {
                let events = sink.events.clone();
                let opened = self.compositor.borrow_mut().open_video(id, bytes, sink);
                if let Err(error) = opened {
                    events(VideoEvent::Failed(error.to_string()));
                }
            }
            VideoRequest::Control { id, control } => {
                self.compositor.borrow_mut().control_video(id, control);
                return;
            }
            VideoRequest::Extent { id, width, height } => {
                self.video_extents
                    .insert(id, Extent::new(width.max(1), height.max(1)));
            }
            VideoRequest::Close { id } => {
                self.compositor.borrow_mut().close_video(id);
                self.video_extents.remove(&id);
            }
        }
        self.mark_composition_dirty();
    }
}
