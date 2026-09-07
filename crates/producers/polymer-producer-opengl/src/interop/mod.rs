//! Common OpenGL definitions and platform-specific GPU interop backends.

pub(crate) mod memory_object_win32;

use std::{ffi::c_void, mem};

pub(crate) type Hdc = *mut c_void;
pub(crate) type Hglrc = *mut c_void;
pub(crate) type Gluint = u32;
pub(crate) type Glenum = u32;
pub(crate) type Glint = i32;
pub(crate) type Glsizei = i32;

pub(crate) const GL_TEXTURE_2D: Glenum = 0x0DE1;
pub(crate) const GL_TEXTURE_BINDING_2D: Glenum = 0x8069;
pub(crate) const GL_RGBA8: Glenum = 0x8058;
pub(crate) const GL_READ_FRAMEBUFFER: Glenum = 0x8CA8;
pub(crate) const GL_DRAW_FRAMEBUFFER: Glenum = 0x8CA9;
pub(crate) const GL_READ_FRAMEBUFFER_BINDING: Glenum = 0x8CAA;
pub(crate) const GL_DRAW_FRAMEBUFFER_BINDING: Glenum = 0x8CA6;
pub(crate) const GL_COLOR_ATTACHMENT0: Glenum = 0x8CE0;
pub(crate) const GL_COLOR_BUFFER_BIT: u32 = 0x0000_4000;
pub(crate) const GL_NEAREST: Glenum = 0x2600;
pub(crate) const GL_SCISSOR_TEST: Glenum = 0x0C11;
pub(crate) const GL_FRAMEBUFFER_SRGB: Glenum = 0x8DB9;

#[link(name = "opengl32")]
unsafe extern "system" {
    fn wglGetCurrentContext() -> Hglrc;
    pub(crate) fn wglGetCurrentDC() -> Hdc;
    fn wglGetProcAddress(name: *const u8) -> *const c_void;
    pub(crate) fn glGenTextures(count: Glsizei, textures: *mut Gluint);
    pub(crate) fn glDeleteTextures(count: Glsizei, textures: *const Gluint);
    pub(crate) fn glBindTexture(target: Glenum, texture: Gluint);
    pub(crate) fn glGetIntegerv(name: Glenum, value: *mut Glint);
    pub(crate) fn glIsEnabled(capability: Glenum) -> u8;
    pub(crate) fn glEnable(capability: Glenum);
    pub(crate) fn glDisable(capability: Glenum);
    pub(crate) fn glReadBuffer(buffer: Glenum);

}

pub(crate) type GlGenFramebuffers = unsafe extern "system" fn(Glsizei, *mut Gluint);
pub(crate) type GlDeleteFramebuffers = unsafe extern "system" fn(Glsizei, *const Gluint);
pub(crate) type GlBindFramebuffer = unsafe extern "system" fn(Glenum, Gluint);
pub(crate) type GlFramebufferTexture2D =
    unsafe extern "system" fn(Glenum, Glenum, Glenum, Gluint, Glint);
pub(crate) type GlBlitFramebuffer =
    unsafe extern "system" fn(Glint, Glint, Glint, Glint, Glint, Glint, Glint, Glint, u32, Glenum);

pub(crate) unsafe fn proc<T: Copy>(name: &'static [u8]) -> Option<T> {
    let address = unsafe { wglGetProcAddress(name.as_ptr()) };
    (!matches!(address as usize, 0 | 1 | 2 | 3 | usize::MAX))
        .then(|| unsafe { mem::transmute_copy(&address) })
}

pub(crate) unsafe fn current_context() -> Hglrc {
    unsafe { wglGetCurrentContext() }
}

pub(crate) const GL_READ_BUFFER: Glenum = 0x0C02;
pub(crate) const GL_DOUBLEBUFFER: Glenum = 0x0C32;
pub(crate) const GL_BACK: Glenum = 0x0405;
pub(crate) const GL_FRONT: Glenum = 0x0404;
