//! `GL_EXT_memory_object_win32` D3D12 external-memory interop.

use super::{
    GL_RGBA8, GL_TEXTURE_2D, GL_TEXTURE_BINDING_2D, GlBindFramebuffer, GlBlitFramebuffer,
    GlDeleteFramebuffers, GlFramebufferTexture2D, GlFramebufferTextureLayer, GlGenFramebuffers,
    Glenum, Glint, Glsizei, Gluint, glBindTexture, glDeleteTextures, glGenTextures, glGetIntegerv,
    proc,
};
use polymer_protocol::AdapterLuid;
use std::ffi::c_void;
use windows::Win32::{
    Foundation::{CloseHandle, GENERIC_ALL, HANDLE},
    Graphics::{
        Direct3D::D3D_FEATURE_LEVEL_11_0,
        Direct3D12::{
            D3D12_CPU_PAGE_PROPERTY_UNKNOWN, D3D12_HEAP_FLAG_SHARED, D3D12_HEAP_PROPERTIES,
            D3D12_HEAP_TYPE_DEFAULT, D3D12_MEMORY_POOL_UNKNOWN, D3D12_RESOURCE_DESC,
            D3D12_RESOURCE_DIMENSION_TEXTURE2D, D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
            D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS, D3D12_RESOURCE_STATE_COMMON,
            D3D12_TEXTURE_LAYOUT_UNKNOWN, D3D12CreateDevice, ID3D12Device, ID3D12Resource,
        },
        Dxgi::{
            Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC},
            CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, IDXGIAdapter1, IDXGIFactory4,
        },
    },
};

type Gluint64 = u64;

pub(crate) const GL_DEVICE_LUID_EXT: Glenum = 0x9599;
const GL_DEDICATED_MEMORY_OBJECT_EXT: Glenum = 0x9581;
const GL_HANDLE_TYPE_D3D12_RESOURCE_EXT: Glenum = 0x958A;
pub(crate) const GL_HANDLE_TYPE_D3D12_FENCE_EXT: Glenum = 0x9594;
pub(crate) const GL_D3D12_FENCE_VALUE_EXT: Glenum = 0x9595;
pub(crate) const GL_LAYOUT_GENERAL_EXT: Glenum = 0x958D;

type GlGetUnsignedBytev = unsafe extern "system" fn(Glenum, *mut u8);
type GlCreateMemoryObjects = unsafe extern "system" fn(Glsizei, *mut Gluint);
type GlDeleteMemoryObjects = unsafe extern "system" fn(Glsizei, *const Gluint);
type GlMemoryObjectParameteriv = unsafe extern "system" fn(Gluint, Glenum, *const Glint);
type GlImportMemoryWin32Handle = unsafe extern "system" fn(Gluint, Gluint64, Glenum, *mut c_void);
type GlTexStorageMem2D =
    unsafe extern "system" fn(Glenum, Glsizei, Glenum, Glsizei, Glsizei, Gluint, Gluint64);
type GlGenSemaphores = unsafe extern "system" fn(Glsizei, *mut Gluint);
type GlDeleteSemaphores = unsafe extern "system" fn(Glsizei, *const Gluint);
type GlImportSemaphoreWin32Handle = unsafe extern "system" fn(Gluint, Glenum, *mut c_void);
type GlSemaphoreParameterui64v = unsafe extern "system" fn(Gluint, Glenum, *const Gluint64);
type GlSignalSemaphore =
    unsafe extern "system" fn(Gluint, Gluint, *const Gluint, Gluint, *const Gluint, *const Glenum);

#[derive(Clone, Copy)]
pub(crate) struct GlFunctions {
    pub(crate) get_unsigned_byte: GlGetUnsignedBytev,
    pub(crate) gen_framebuffers: GlGenFramebuffers,
    pub(crate) delete_framebuffers: GlDeleteFramebuffers,
    pub(crate) bind_framebuffer: GlBindFramebuffer,
    pub(crate) framebuffer_texture_2d: GlFramebufferTexture2D,
    pub(crate) framebuffer_texture_layer: GlFramebufferTextureLayer,
    pub(crate) blit_framebuffer: GlBlitFramebuffer,
    create_memory_objects: GlCreateMemoryObjects,
    pub(crate) delete_memory_objects: GlDeleteMemoryObjects,
    memory_object_parameter: GlMemoryObjectParameteriv,
    import_memory: GlImportMemoryWin32Handle,
    tex_storage_mem_2d: GlTexStorageMem2D,
    pub(crate) gen_semaphores: GlGenSemaphores,
    pub(crate) delete_semaphores: GlDeleteSemaphores,
    pub(crate) import_semaphore: GlImportSemaphoreWin32Handle,
    pub(crate) semaphore_parameter: GlSemaphoreParameterui64v,
    pub(crate) signal_semaphore: GlSignalSemaphore,
}

impl GlFunctions {
    pub(crate) unsafe fn load() -> Option<Self> {
        Some(Self {
            get_unsigned_byte: unsafe { proc(b"glGetUnsignedBytevEXT\0")? },
            gen_framebuffers: unsafe { proc(b"glGenFramebuffers\0")? },
            delete_framebuffers: unsafe { proc(b"glDeleteFramebuffers\0")? },
            bind_framebuffer: unsafe { proc(b"glBindFramebuffer\0")? },
            framebuffer_texture_2d: unsafe { proc(b"glFramebufferTexture2D\0")? },
            framebuffer_texture_layer: unsafe { proc(b"glFramebufferTextureLayer\0")? },
            blit_framebuffer: unsafe { proc(b"glBlitFramebuffer\0")? },
            create_memory_objects: unsafe { proc(b"glCreateMemoryObjectsEXT\0")? },
            delete_memory_objects: unsafe { proc(b"glDeleteMemoryObjectsEXT\0")? },
            memory_object_parameter: unsafe { proc(b"glMemoryObjectParameterivEXT\0")? },
            import_memory: unsafe { proc(b"glImportMemoryWin32HandleEXT\0")? },
            tex_storage_mem_2d: unsafe { proc(b"glTexStorageMem2DEXT\0")? },
            gen_semaphores: unsafe { proc(b"glGenSemaphoresEXT\0")? },
            delete_semaphores: unsafe { proc(b"glDeleteSemaphoresEXT\0")? },
            import_semaphore: unsafe { proc(b"glImportSemaphoreWin32HandleEXT\0")? },
            semaphore_parameter: unsafe { proc(b"glSemaphoreParameterui64vEXT\0")? },
            signal_semaphore: unsafe { proc(b"glSignalSemaphoreEXT\0")? },
        })
    }
}

pub(crate) struct ExternalImage {
    _resource: ID3D12Resource,
    pub(crate) handle: HANDLE,
    memory: Gluint,
    pub(crate) texture: Gluint,
    functions: GlFunctions,
}

unsafe impl Send for ExternalImage {}

impl ExternalImage {
    pub(crate) unsafe fn create_device(
        luid: AdapterLuid,
    ) -> Result<(ID3D12Device, IDXGIAdapter1), String> {
        let factory = unsafe { CreateDXGIFactory2::<IDXGIFactory4>(DXGI_CREATE_FACTORY_FLAGS(0)) }
            .map_err(|error| format!("CreateDXGIFactory2: {error}"))?;
        for index in 0.. {
            let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
                break;
            };
            let desc = unsafe { adapter.GetDesc1() }
                .map_err(|error| format!("IDXGIAdapter1::GetDesc1: {error}"))?;
            if desc.AdapterLuid.LowPart != luid.low_part
                || desc.AdapterLuid.HighPart != luid.high_part
            {
                continue;
            }
            let mut device = None;
            unsafe { D3D12CreateDevice(&adapter, D3D_FEATURE_LEVEL_11_0, &mut device) }
                .map_err(|error| format!("D3D12CreateDevice: {error}"))?;
            return device
                .map(|device| (device, adapter))
                .ok_or_else(|| "D3D12CreateDevice returned no device".into());
        }
        Err(format!(
            "no DXGI adapter matched OpenGL LUID {:08x}:{:08x}",
            luid.high_part as u32, luid.low_part
        ))
    }
}

impl ExternalImage {
    pub(crate) unsafe fn new(
        device: &ID3D12Device,
        functions: &GlFunctions,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        let heap = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_DEFAULT,
            CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
            MemoryPoolPreference: D3D12_MEMORY_POOL_UNKNOWN,
            CreationNodeMask: 1,
            VisibleNodeMask: 1,
        };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Alignment: 0,
            Width: width as u64,
            Height: height,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
            Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET
                | D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS,
        };
        let mut resource = None;
        unsafe {
            device.CreateCommittedResource(
                &heap,
                D3D12_HEAP_FLAG_SHARED,
                &desc,
                D3D12_RESOURCE_STATE_COMMON,
                None,
                &mut resource,
            )
        }
        .map_err(|error| format!("ID3D12Device::CreateCommittedResource: {error}"))?;
        let resource = resource.ok_or("CreateCommittedResource returned no resource")?;
        let handle = unsafe { device.CreateSharedHandle(&resource, None, GENERIC_ALL.0, None) }
            .map_err(|error| format!("CreateSharedHandle(resource): {error}"))?;
        let mut memory = 0;
        let mut texture = 0;
        let dedicated = 1;
        let mut old_texture = 0;
        unsafe {
            (functions.create_memory_objects)(1, &mut memory);
            (functions.memory_object_parameter)(memory, GL_DEDICATED_MEMORY_OBJECT_EXT, &dedicated);
            (functions.import_memory)(memory, 0, GL_HANDLE_TYPE_D3D12_RESOURCE_EXT, handle.0);
            glGetIntegerv(GL_TEXTURE_BINDING_2D, &mut old_texture);
            glGenTextures(1, &mut texture);
            glBindTexture(GL_TEXTURE_2D, texture);
            (functions.tex_storage_mem_2d)(
                GL_TEXTURE_2D,
                1,
                GL_RGBA8,
                width as i32,
                height as i32,
                memory,
                0,
            );
            glBindTexture(GL_TEXTURE_2D, old_texture as u32);
        }
        Ok(Self {
            _resource: resource,
            handle,
            memory,
            texture,
            functions: *functions,
        })
    }
}

impl Drop for ExternalImage {
    fn drop(&mut self) {
        unsafe {
            glDeleteTextures(1, &self.texture);
            (self.functions.delete_memory_objects)(1, &self.memory);
            let _ = CloseHandle(self.handle);
        }
    }
}
