//! `WGL_NV_DX_interop` transport used when EXT external objects are absent.

use super::{
    GlBindFramebuffer, GlBlitFramebuffer, GlDeleteFramebuffers, GlFramebufferTexture2D,
    GlFramebufferTextureLayer, GlGenFramebuffers, Glenum, Glint, Gluint,
};
use crate::interop;
use snowglass_producer_d3d11::{D3d11ImageRing, D3d11RingMode};
use snowglass_protocol::AdapterLuid;
use std::ffi::c_void;
use windows::{
    Win32::Graphics::{
        Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0},
        Direct3D11::{
            D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11CreateDevice,
            ID3D11Device,
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
}

unsafe impl Send for Slot {}

pub(crate) struct Ring {
    pub functions: GlFunctions,
    pub device_handle: *mut c_void,
    pub ring: D3d11ImageRing,
    pub slots: Vec<Slot>,
    pub adapter_luid: AdapterLuid,
}

unsafe impl Send for Ring {}

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
            return unsafe {
                Self::finish(
                    device,
                    adapter,
                    device_handle,
                    functions,
                    register_object,
                    width,
                    height,
                )
            };
        }
        Err("WGL_NV_DX_interop rejected every DXGI adapter".into())
    }

    unsafe fn finish(
        device: ID3D11Device,
        adapter: IDXGIAdapter1,
        device_handle: *mut c_void,
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
        let mut slots = Vec::with_capacity(3);
        for image in ring.slots() {
            let mut texture = 0;
            unsafe { interop::glGenTextures(1, &mut texture) };
            let object = unsafe {
                register_object(
                    device_handle,
                    Interface::as_raw(&image.texture),
                    texture,
                    interop::GL_TEXTURE_2D,
                    WGL_ACCESS_WRITE_DISCARD_NV,
                )
            };
            if object.is_null() {
                unsafe { interop::glDeleteTextures(1, &texture) };
                return Err("wglDXRegisterObjectNV failed".into());
            }
            slots.push(Slot {
                texture,
                object,
                keyed_mutex: image.keyed_mutex.as_ref().expect("keyed ring").clone(),
            });
        }
        let adapter_desc = unsafe { adapter.GetDesc1() }.map_err(|e| format!("GetDesc1: {e}"))?;
        Ok(Self {
            functions,
            device_handle,
            ring,
            slots,
            adapter_luid: AdapterLuid {
                low_part: adapter_desc.AdapterLuid.LowPart,
                high_part: adapter_desc.AdapterLuid.HighPart,
            },
        })
    }

    pub(crate) unsafe fn acquire(&self, index: usize) -> Result<(), String> {
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
        if unsafe { (self.functions.lock_objects)(self.device_handle, 1, &slot.object) } == 0 {
            let _ = unsafe { slot.keyed_mutex.ReleaseSync(0) };
            return Err("wglDXLockObjectsNV failed".into());
        }
        Ok(())
    }

    pub(crate) unsafe fn release(&self, index: usize) -> Result<(), String> {
        let slot = &self.slots[index];
        if unsafe { (self.functions.unlock_objects)(self.device_handle, 1, &slot.object) } == 0 {
            let _ = unsafe { slot.keyed_mutex.ReleaseSync(0) };
            return Err("wglDXUnlockObjectsNV failed".into());
        }
        unsafe { slot.keyed_mutex.ReleaseSync(1) }
            .map_err(|e| format!("IDXGIKeyedMutex::ReleaseSync: {e}"))
    }
}

impl Drop for Ring {
    fn drop(&mut self) {
        unsafe {
            for slot in &self.slots {
                let _ = (self.functions.unregister_object)(self.device_handle, slot.object);
                interop::glDeleteTextures(1, &slot.texture);
            }
            let _ = (self.functions.close_device)(self.device_handle);
        }
    }
}
