//! `WGL_NV_DX_interop` transport used when EXT external objects are absent.

use super::{
    GlBindFramebuffer, GlBlitFramebuffer, GlDeleteFramebuffers, GlFramebufferTexture2D,
    GlFramebufferTextureLayer, GlGenFramebuffers, Glenum, Glint, Gluint,
};
use crate::interop;
use polymer_producer_d3d11::{D3d11ImageRing, D3d11RingMode};
use polymer_protocol::AdapterLuid;
use std::ffi::c_void;
use windows::{
    Win32::Graphics::{
        Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0},
        Direct3D11::{
            D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11CreateDevice,
            ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
        },
        Dxgi::{
            Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC},
            CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, IDXGIAdapter1, IDXGIFactory4,
            IDXGIKeyedMutex,
        },
    },
    core::Interface,
};

const WGL_ACCESS_WRITE_DISCARD_NV: Glenum = 0x0002;

type WglDxOpenDevice = unsafe extern "system" fn(*mut c_void) -> *mut c_void;
type WglDxCloseDevice = unsafe extern "system" fn(*mut c_void) -> i32;
type WglDxRegisterObject =
    unsafe extern "system" fn(*mut c_void, *mut c_void, Gluint, Glenum, Glenum) -> *mut c_void;
type WglDxUnregisterObject = unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32;
type WglDxLockObjects = unsafe extern "system" fn(*mut c_void, Glint, *const *mut c_void) -> i32;
type WglDxUnlockObjects = unsafe extern "system" fn(*mut c_void, Glint, *const *mut c_void) -> i32;

#[derive(Clone, Copy)]
pub(crate) struct GlFunctions {
    pub gen_framebuffers: GlGenFramebuffers,
    pub delete_framebuffers: GlDeleteFramebuffers,
    pub bind_framebuffer: GlBindFramebuffer,
    pub framebuffer_texture_2d: GlFramebufferTexture2D,
    pub framebuffer_texture_layer: GlFramebufferTextureLayer,
    pub framebuffer_renderbuffer: super::GlFramebufferRenderbuffer,
    pub blit_framebuffer: GlBlitFramebuffer,
    close_device: WglDxCloseDevice,
    unregister_object: WglDxUnregisterObject,
    lock_objects: WglDxLockObjects,
    unlock_objects: WglDxUnlockObjects,
}

unsafe fn load<T: Copy>(name: &'static [u8]) -> Option<T> {
    unsafe { interop::proc(name) }
}

pub(crate) struct Slot {
    pub texture: Gluint,
    pub object: *mut c_void,
    pub keyed_mutex: IDXGIKeyedMutex,
    image: ID3D11Texture2D,
}

unsafe impl Send for Slot {}

pub(crate) struct Ring {
    pub functions: GlFunctions,
    device_handle: DeviceHandle,
    pub ring: D3d11ImageRing,
    pub slots: Vec<Slot>,
    pub adapter_luid: AdapterLuid,
    context: ID3D11DeviceContext,
}

unsafe impl Send for Ring {}

struct DeviceHandle {
    raw: *mut c_void,
    close: WglDxCloseDevice,
}
impl Drop for DeviceHandle {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { (self.close)(self.raw) };
        }
    }
}

impl Ring {
    pub(crate) unsafe fn new(width: u32, height: u32) -> Result<Self, String> {
        let open_device: WglDxOpenDevice =
            unsafe { load(b"wglDXOpenDeviceNV\0") }.ok_or("WGL_NV_DX_interop is unavailable")?;
        let register_object: WglDxRegisterObject =
            unsafe { load(b"wglDXRegisterObjectNV\0") }.ok_or("WGL_NV_DX_interop is incomplete")?;
        let functions = GlFunctions {
            gen_framebuffers: unsafe { load(b"glGenFramebuffers\0") }
                .ok_or("missing glGenFramebuffers")?,
            delete_framebuffers: unsafe { load(b"glDeleteFramebuffers\0") }
                .ok_or("missing glDeleteFramebuffers")?,
            bind_framebuffer: unsafe { load(b"glBindFramebuffer\0") }
                .ok_or("missing glBindFramebuffer")?,
            framebuffer_texture_2d: unsafe { load(b"glFramebufferTexture2D\0") }
                .ok_or("missing glFramebufferTexture2D")?,
            framebuffer_renderbuffer: unsafe { load(b"glFramebufferRenderbuffer\0") }
                .ok_or("missing glFramebufferRenderbuffer")?,
            framebuffer_texture_layer: unsafe { load(b"glFramebufferTextureLayer\0") }
                .ok_or("missing glFramebufferTextureLayer")?,
            blit_framebuffer: unsafe { load(b"glBlitFramebuffer\0") }
                .ok_or("missing glBlitFramebuffer")?,
            close_device: unsafe { load(b"wglDXCloseDeviceNV\0") }
                .ok_or("missing wglDXCloseDeviceNV")?,
            unregister_object: unsafe { load(b"wglDXUnregisterObjectNV\0") }
                .ok_or("missing wglDXUnregisterObjectNV")?,
            lock_objects: unsafe { load(b"wglDXLockObjectsNV\0") }
                .ok_or("missing wglDXLockObjectsNV")?,
            unlock_objects: unsafe { load(b"wglDXUnlockObjectsNV\0") }
                .ok_or("missing wglDXUnlockObjectsNV")?,
        };

        let factory = unsafe { CreateDXGIFactory2::<IDXGIFactory4>(DXGI_CREATE_FACTORY_FLAGS(0)) }
            .map_err(|e| format!("CreateDXGIFactory2: {e}"))?;
        let mut failure = "WGL_NV_DX_interop rejected every DXGI adapter".to_string();
        for index in 0.. {
            let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
                break;
            };
            let mut device = None;
            let result = unsafe {
                D3D11CreateDevice(
                    &adapter,
                    D3D_DRIVER_TYPE_UNKNOWN,
                    Default::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    Some(&[D3D_FEATURE_LEVEL_11_0]),
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    None,
                )
            };
            let Ok(()) = result else { continue };
            let Some(device) = device else { continue };
            let device_handle = unsafe { open_device(Interface::as_raw(&device)) };
            if device_handle.is_null() {
                continue;
            }
            let attempt = unsafe {
                Self::finish(
                    device,
                    adapter,
                    DeviceHandle {
                        raw: device_handle,
                        close: functions.close_device,
                    },
                    functions,
                    register_object,
                    width,
                    height,
                )
            };
            match attempt {
                Ok(ring) => return Ok(ring),
                Err(error) => failure = error,
            }
        }
        Err(failure)
    }

    unsafe fn finish(
        device: ID3D11Device,
        adapter: IDXGIAdapter1,
        device_handle: DeviceHandle,
        functions: GlFunctions,
        register_object: WglDxRegisterObject,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE).0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let ring =
            unsafe { D3d11ImageRing::new(&device, &desc, 3, D3d11RingMode::KeyedMutexNoFence) }
                .map_err(|e| format!("D3D11 interop ring: {e}"))?;
        let adapter_desc = unsafe { adapter.GetDesc1() }.map_err(|e| format!("GetDesc1: {e}"))?;
        let mut capture = Self {
            functions,
            device_handle,
            ring,
            slots: Vec::with_capacity(3),
            adapter_luid: AdapterLuid {
                low_part: adapter_desc.AdapterLuid.LowPart,
                high_part: adapter_desc.AdapterLuid.HighPart,
            },
            context: unsafe { device.GetImmediateContext() }.map_err(|error| error.to_string())?,
        };
        for image in capture.ring.slots() {
            // WGL registers a private D3D image. The exported NT-handle image
            // has separate consumer ownership and receives a native copy after
            // GL releases the interop object.
            // Direct registration of the exported NT texture fails on the tested
            // NVIDIA driver; see this crate's INTEROP.md for probe results.
            let mut interop_image = None;
            unsafe { device.CreateTexture2D(&desc, None, Some(&mut interop_image)) }
                .map_err(|error| format!("D3D11 interop image: {error}"))?;
            let interop_image = interop_image.ok_or("missing D3D11 interop image")?;
            let mut texture = 0;
            unsafe { interop::glGenTextures(1, &mut texture) };
            let object = unsafe {
                register_object(
                    capture.device_handle.raw,
                    Interface::as_raw(&interop_image),
                    texture,
                    interop::GL_TEXTURE_2D,
                    WGL_ACCESS_WRITE_DISCARD_NV,
                )
            };
            if object.is_null() {
                let error = windows::core::Error::from_win32();
                unsafe { interop::glDeleteTextures(1, &texture) };
                return Err(format!("wglDXRegisterObjectNV failed: {error}"));
            }
            capture.slots.push(Slot {
                texture,
                object,
                keyed_mutex: image.keyed_mutex.as_ref().expect("keyed ring").clone(),
                image: interop_image,
            });
        }
        Ok(capture)
    }

    pub(crate) unsafe fn acquire(&self, index: usize) -> Result<Lease<'_>, String> {
        let slot = &self.slots[index];
        let hr = unsafe {
            (Interface::vtable(&slot.keyed_mutex).AcquireSync)(
                Interface::as_raw(&slot.keyed_mutex),
                0,
                0,
            )
        };
        if hr.0 != 0 {
            return Err("interop slot is still owned by the viewer".into());
        }
        if unsafe { (self.functions.lock_objects)(self.device_handle.raw, 1, &slot.object) } == 0 {
            let _ = unsafe { slot.keyed_mutex.ReleaseSync(0) };
            return Err("wglDXLockObjectsNV failed".into());
        }
        Ok(Lease {
            ring: self,
            index,
            locked: true,
            owned: true,
        })
    }
}

/// Both WGL object access and keyed-mutex ownership belong to the lease. It
/// must be completed or dropped on the same presenting OpenGL context.
pub(crate) struct Lease<'a> {
    ring: &'a Ring,
    index: usize,
    locked: bool,
    owned: bool,
}
impl Lease<'_> {
    fn unlock(&mut self) -> Result<(), String> {
        if self.locked {
            let slot = &self.ring.slots[self.index];
            if unsafe {
                (self.ring.functions.unlock_objects)(self.ring.device_handle.raw, 1, &slot.object)
            } == 0
            {
                return Err("wglDXUnlockObjectsNV failed".into());
            }
            self.locked = false;
        }
        Ok(())
    }
}
impl Lease<'_> {
    pub fn finish(mut self) -> Result<(), String> {
        self.unlock()?;
        unsafe {
            self.ring.context.CopyResource(
                &self.ring.ring.slots()[self.index].texture,
                &self.ring.slots[self.index].image,
            );
        }
        unsafe { self.ring.slots[self.index].keyed_mutex.ReleaseSync(1) }
            .map_err(|error| format!("IDXGIKeyedMutex::ReleaseSync: {error}"))?;
        self.owned = false;
        Ok(())
    }
}
impl Drop for Lease<'_> {
    fn drop(&mut self) {
        if self.owned && self.unlock().is_ok() {
            let _ = unsafe { self.ring.slots[self.index].keyed_mutex.ReleaseSync(0) };
        }
    }
}

impl Ring {
    /// Retire independent registrations and preserve any still owned by GL.
    /// Runs only with the registration's original context current.
    fn unregister_slots(&mut self) {
        unsafe {
            self.slots.retain(|slot| {
                if (self.functions.unregister_object)(self.device_handle.raw, slot.object) != 0 {
                    interop::glDeleteTextures(1, &slot.texture);
                    false
                } else {
                    true
                }
            });
        }
    }
}

impl Drop for Ring {
    fn drop(&mut self) {
        self.unregister_slots();
        if !self.slots.is_empty() {
            // A failed unlock can leave a registration GL-owned. Keep its
            // native image and device connection alive together.
            std::mem::forget(std::mem::take(&mut self.slots));
            // Closing the device would invalidate the retained registration.
            self.device_handle.raw = std::ptr::null_mut();
            tracing::error!("polymer: retaining NV interop resources after unregister failure");
        }
    }
}
