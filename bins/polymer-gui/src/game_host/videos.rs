use super::*;
use crate::shell::protocol::VideoRequest;
use polymer_composition::VideoEvent;

impl GameHost {
    pub fn handle_video(&mut self, id: u64, request: VideoRequest) {
        match request {
            VideoRequest::Open { source, sink } => {
                let events = sink.events.clone();
                let opened = self.compositor.borrow_mut().open_video(id, source, sink);
                if let Err(error) = opened {
                    events(VideoEvent::Failed(error.to_string()));
                }
            }
            VideoRequest::Control(control) => {
                self.compositor.borrow_mut().control_video(id, control);
                return;
            }
            VideoRequest::Extent { width, height } => {
                self.video_extents
                    .insert(id, Extent::new(width.max(1), height.max(1)));
            }
            VideoRequest::Close => {
                self.compositor.borrow_mut().close_video(id);
                self.video_extents.remove(&id);
            }
        }
        self.mark_composition_dirty();
    }
}
