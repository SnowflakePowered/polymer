use crate::interop::{GL_RENDERBUFFER, GlFramebufferRenderbuffer};
use crate::{
    interop::{
        GL_COLOR_ATTACHMENT0, GL_READ_FRAMEBUFFER, GL_TEXTURE_2D_ARRAY, GlFramebufferTexture2D,
        GlFramebufferTextureLayer,
    },
    tracking::DrawCandidate,
};

impl DrawCandidate {
    /// Attach the validated source subresource to the transport's read FBO.
    ///
    /// # Safety
    /// The source's context is current and the transport's read FBO is bound.
    pub(super) unsafe fn attach_read_framebuffer(
        &self,
        texture_2d: GlFramebufferTexture2D,
        texture_layer: GlFramebufferTextureLayer,
        renderbuffer: GlFramebufferRenderbuffer,
    ) {
        if self.target == 0 && self.texture == 0 {
            return;
        }
        unsafe {
            if self.target == GL_RENDERBUFFER {
                renderbuffer(
                    GL_READ_FRAMEBUFFER,
                    GL_COLOR_ATTACHMENT0,
                    self.target,
                    self.texture,
                );
            } else if self.target == GL_TEXTURE_2D_ARRAY {
                texture_layer(
                    GL_READ_FRAMEBUFFER,
                    GL_COLOR_ATTACHMENT0,
                    self.texture,
                    self.mip,
                    self.layer,
                );
            } else {
                texture_2d(
                    GL_READ_FRAMEBUFFER,
                    GL_COLOR_ATTACHMENT0,
                    self.target,
                    self.texture,
                    self.mip,
                );
            }
        }
    }
}
