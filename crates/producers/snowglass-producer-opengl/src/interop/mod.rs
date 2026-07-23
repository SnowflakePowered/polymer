//! Common OpenGL definitions and platform-specific GPU interop backends.

pub(crate) mod memory_object_win32;
pub(crate) mod nv_dx;

use std::{ffi::c_void, mem};

pub(crate) type Hdc = *mut c_void;
pub(crate) type Hglrc = *mut c_void;
pub(crate) type Gluint = u32;
pub(crate) type Glenum = u32;
pub(crate) type Glint = i32;
pub(crate) type Glsizei = i32;

pub(crate) const GL_TEXTURE_2D: Glenum = 0x0DE1;
pub(crate) const GL_TEXTURE_2D_ARRAY: Glenum = 0x8C1A;
pub(crate) const GL_TEXTURE_BINDING_2D: Glenum = 0x8069;
pub(crate) const GL_TEXTURE_BINDING_2D_ARRAY: Glenum = 0x8C1D;
pub(crate) const GL_TEXTURE_WIDTH: Glenum = 0x1000;
pub(crate) const GL_TEXTURE_HEIGHT: Glenum = 0x1001;
pub(crate) const GL_RGBA8: Glenum = 0x8058;
pub(crate) const GL_READ_FRAMEBUFFER: Glenum = 0x8CA8;
pub(crate) const GL_DRAW_FRAMEBUFFER: Glenum = 0x8CA9;
pub(crate) const GL_READ_FRAMEBUFFER_BINDING: Glenum = 0x8CAA;
pub(crate) const GL_DRAW_FRAMEBUFFER_BINDING: Glenum = 0x8CA6;
pub(crate) const GL_COLOR_ATTACHMENT0: Glenum = 0x8CE0;
pub(crate) const GL_COLOR_BUFFER_BIT: u32 = 0x0000_4000;
pub(crate) const GL_NEAREST: Glenum = 0x2600;
pub(crate) const GL_VIEWPORT: Glenum = 0x0BA2;
pub(crate) const GL_SCISSOR_TEST: Glenum = 0x0C11;
pub(crate) const GL_SCISSOR_BOX: Glenum = 0x0C10;
pub(crate) const GL_ACTIVE_TEXTURE: Glenum = 0x84E0;
pub(crate) const GL_TEXTURE0: Glenum = 0x84C0;

#[link(name = "opengl32")]
unsafe extern "system" {
    fn wglGetCurrentContext() -> Hglrc;
    fn wglGetProcAddress(name: *const u8) -> *const c_void;
    pub(crate) fn glGenTextures(count: Glsizei, textures: *mut Gluint);
    pub(crate) fn glDeleteTextures(count: Glsizei, textures: *const Gluint);
    pub(crate) fn glBindTexture(target: Glenum, texture: Gluint);
    pub(crate) fn glGetIntegerv(name: Glenum, value: *mut Glint);
    pub(crate) fn glGetTexLevelParameteriv(
        target: Glenum,
        level: Glint,
        name: Glenum,
        value: *mut Glint,
    );
    pub(crate) fn glIsEnabled(capability: Glenum) -> u8;
    pub(crate) fn glEnable(capability: Glenum);
    pub(crate) fn glDisable(capability: Glenum);
}

pub(crate) type GlActiveTexture = unsafe extern "system" fn(Glenum);
pub(crate) type GlGenFramebuffers = unsafe extern "system" fn(Glsizei, *mut Gluint);
pub(crate) type GlDeleteFramebuffers = unsafe extern "system" fn(Glsizei, *const Gluint);
pub(crate) type GlBindFramebuffer = unsafe extern "system" fn(Glenum, Gluint);
pub(crate) type GlFramebufferTexture2D =
    unsafe extern "system" fn(Glenum, Glenum, Glenum, Gluint, Glint);
pub(crate) type GlFramebufferTextureLayer =
    unsafe extern "system" fn(Glenum, Glenum, Gluint, Glint, Glint);
pub(crate) type GlBlitFramebuffer =
    unsafe extern "system" fn(Glint, Glint, Glint, Glint, Glint, Glint, Glint, Glint, u32, Glenum);

pub(crate) unsafe fn proc<T: Copy>(name: &'static [u8]) -> Option<T> {
    let address = unsafe { wglGetProcAddress(name.as_ptr()) };
    (!address.is_null()).then(|| unsafe { mem::transmute_copy(&address) })
}

pub(crate) unsafe fn current_context() -> Hglrc {
    unsafe { wglGetCurrentContext() }
}
