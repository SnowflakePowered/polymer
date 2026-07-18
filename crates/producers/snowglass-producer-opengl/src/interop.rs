//! OpenGL extension loading and D3D12 external-memory interop.

use snowglass_protocol::AdapterLuid;
use std::{ffi::c_void, mem};
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

pub(crate) type Hdc = *mut c_void;
pub(crate) type Hglrc = *mut c_void;
pub(crate) type Gluint = u32;
pub(crate) type Glenum = u32;
pub(crate) type Glint = i32;
pub(crate) type Glsizei = i32;
type Gluint64 = u64;

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
pub(crate) const GL_DEVICE_LUID_EXT: Glenum = 0x9599;
const GL_DEDICATED_MEMORY_OBJECT_EXT: Glenum = 0x9581;
const GL_HANDLE_TYPE_D3D12_RESOURCE_EXT: Glenum = 0x958A;
pub(crate) const GL_HANDLE_TYPE_D3D12_FENCE_EXT: Glenum = 0x9594;
pub(crate) const GL_D3D12_FENCE_VALUE_EXT: Glenum = 0x9595;
pub(crate) const GL_LAYOUT_GENERAL_EXT: Glenum = 0x958D;

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
type GlGetUnsignedBytev = unsafe extern "system" fn(Glenum, *mut u8);
type GlGenFramebuffers = unsafe extern "system" fn(Glsizei, *mut Gluint);
type GlDeleteFramebuffers = unsafe extern "system" fn(Glsizei, *const Gluint);
type GlBindFramebuffer = unsafe extern "system" fn(Glenum, Gluint);
type GlFramebufferTexture2D = unsafe extern "system" fn(Glenum, Glenum, Glenum, Gluint, Glint);
type GlFramebufferTextureLayer = unsafe extern "system" fn(Glenum, Glenum, Gluint, Glint, Glint);
type GlBlitFramebuffer =
    unsafe extern "system" fn(Glint, Glint, Glint, Glint, Glint, Glint, Glint, Glint, u32, Glenum);
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

pub(crate) unsafe fn proc<T: Copy>(name: &'static [u8]) -> Option<T> {
    let address = unsafe { wglGetProcAddress(name.as_ptr()) };
    (!address.is_null()).then(|| unsafe { mem::transmute_copy(&address) })
}

pub(crate) unsafe fn current_context() -> Hglrc {
    unsafe { wglGetCurrentContext() }
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
    pub(crate) memory: Gluint,
    pub(crate) texture: Gluint,
}

unsafe impl Send for ExternalImage {}

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
        if desc.AdapterLuid.LowPart != luid.low_part || desc.AdapterLuid.HighPart != luid.high_part
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

pub(crate) unsafe fn create_image(
    device: &ID3D12Device,
    functions: &GlFunctions,
    width: u32,
    height: u32,
) -> Result<ExternalImage, String> {
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
    Ok(ExternalImage {
        _resource: resource,
        handle,
        memory,
        texture,
    })
}

pub(crate) unsafe fn destroy_image(functions: &GlFunctions, image: &ExternalImage) {
    unsafe {
        glDeleteTextures(1, &image.texture);
        (functions.delete_memory_objects)(1, &image.memory);
        let _ = CloseHandle(image.handle);
    }
}
