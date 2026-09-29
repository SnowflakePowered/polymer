mod native;
pub(crate) type Session = polymer_renderer::webrtc::EncoderSession<native::Encoder>;
