//! Asynchronous pixel-pack-buffer readback into a D3D11 shared image ring.

use super::{Glenum, Glint, Glsizei, Gluint, proc};
use polymer_producer_d3d11::{D3d11ImageRing, D3d11RingMode};
use polymer_protocol::AdapterLuid;
use std::{ffi::c_void, ptr};
use windows::Win32::Graphics::{
    Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0},
    Direct3D11::{
        D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
        D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11CreateDevice, ID3D11DeviceContext,
    },
    Dxgi::{
        Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC},
        CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, IDXGIFactory4,
    },
};

const GL_PIXEL_PACK_BUFFER: Glenum = 0x88EB;
const GL_PIXEL_PACK_BUFFER_BINDING: Glenum = 0x88ED;
const GL_STREAM_READ: Glenum = 0x88E1;
const GL_READ_ONLY: Glenum = 0x88B8;

type GlGenBuffers = unsafe extern "system" fn(Glsizei, *mut Gluint);
type GlDeleteBuffers = unsafe extern "system" fn(Glsizei, *const Gluint);
type GlBindBuffer = unsafe extern "system" fn(Glenum, Gluint);
type GlBufferData = unsafe extern "system" fn(Glenum, isize, *const c_void, Glenum);
type GlMapBuffer = unsafe extern "system" fn(Glenum, Glenum) -> *mut c_void;
type GlUnmapBuffer = unsafe extern "system" fn(Glenum) -> u8;
type GlReadPixels =
    unsafe extern "system" fn(Glint, Glint, Glsizei, Glsizei, Glenum, Glenum, *mut c_void);

#[derive(Clone, Copy)]
pub(crate) struct GlFunctions {
    gen_buffers: GlGenBuffers,
    delete_buffers: GlDeleteBuffers,
    bind_buffer: GlBindBuffer,
    buffer_data: GlBufferData,
    map_buffer: GlMapBuffer,
    unmap_buffer: GlUnmapBuffer,
    read_pixels: GlReadPixels,
}

impl GlFunctions {
    unsafe fn load() -> Option<Self> {
        Some(Self {
            gen_buffers: unsafe { proc(b"glGenBuffers\0")? },
            delete_buffers: unsafe { proc(b"glDeleteBuffers\0")? },
            bind_buffer: unsafe { proc(b"glBindBuffer\0")? },
            buffer_data: unsafe { proc(b"glBufferData\0")? },
            map_buffer: unsafe { proc(b"glMapBuffer\0")? },
            unmap_buffer: unsafe { proc(b"glUnmapBuffer\0")? },
            read_pixels: super::glReadPixels,
        })
    }
}

pub(crate) struct Transport {
    functions: GlFunctions,
    pub(crate) ring: D3d11ImageRing,
    context: ID3D11DeviceContext,
    pbos: Vec<Gluint>,
    byte_len: usize,
    width: u32,
    height: u32,
    issued: u64,
    next_pbo: usize,
    frames: Vec<Option<(u64, polymer_presentation::producer::FramePlacement)>>,
    scheduler: polymer_presentation::scheduling::SlotScheduler,
    pub(crate) adapter_luid: AdapterLuid,
}

unsafe impl Send for Transport {}

impl Transport {
    pub(crate) unsafe fn new(width: u32, height: u32) -> Result<Self, String> {
        let functions =
            unsafe { GlFunctions::load() }.ok_or("OpenGL pixel buffer objects are unavailable")?;
        let factory = unsafe { CreateDXGIFactory2::<IDXGIFactory4>(DXGI_CREATE_FACTORY_FLAGS(0)) }
            .map_err(|e| format!("CreateDXGIFactory2: {e}"))?;
        for index in 0.. {
            let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
                break;
            };
            let mut device = None;
            if unsafe {
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
            }
            .is_err()
            {
                continue;
            }
            let Some(device) = device else { continue };
            let context = unsafe { device.GetImmediateContext() }
                .map_err(|e| format!("GetImmediateContext: {e}"))?;
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
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let ring =
                unsafe { D3d11ImageRing::new(&device, &desc, 3, D3d11RingMode::KeyedMutexNoFence) }
                    .map_err(|e| format!("D3D11 PBO ring: {e}"))?;
            let byte_len = width as usize * height as usize * 4;
            let mut pbos = vec![0; 3];
            let mut old_pixel_pack_buffer = 0;
            unsafe {
                super::glGetIntegerv(GL_PIXEL_PACK_BUFFER_BINDING, &mut old_pixel_pack_buffer);
                (functions.gen_buffers)(pbos.len() as i32, pbos.as_mut_ptr());
                for &pbo in &pbos {
                    (functions.bind_buffer)(GL_PIXEL_PACK_BUFFER, pbo);
                    (functions.buffer_data)(
                        GL_PIXEL_PACK_BUFFER,
                        byte_len as isize,
                        ptr::null(),
                        GL_STREAM_READ,
                    );
                }
                (functions.bind_buffer)(GL_PIXEL_PACK_BUFFER, old_pixel_pack_buffer as u32);
            }
            let desc = unsafe { adapter.GetDesc1() }.map_err(|e| format!("GetDesc1: {e}"))?;
            return Ok(Self {
                functions,
                ring,
                context,
                pbos,
                byte_len,
                width,
                height,
                issued: 0,
                next_pbo: 0,
                frames: (0..3).map(|_| None).collect(),
                scheduler: Default::default(),
                adapter_luid: AdapterLuid {
                    low_part: desc.AdapterLuid.LowPart,
                    high_part: desc.AdapterLuid.HighPart,
                },
            });
        }
        Err("could not create a D3D11 device for PBO capture".into())
    }

    /// Queues the current framebuffer read and uploads an older completed PBO.
    pub(crate) unsafe fn transfer(
        &mut self,
        x: i32,
        y: i32,
        placement: polymer_presentation::producer::FramePlacement,
    ) -> Result<Option<(usize, u64, polymer_presentation::producer::FramePlacement)>, String> {
        let mut old_pixel_pack_buffer = 0;
        unsafe { super::glGetIntegerv(GL_PIXEL_PACK_BUFFER_BINDING, &mut old_pixel_pack_buffer) };
        let write_index = self.next_pbo;
        self.frames[write_index] = Some((
            polymer_platform_windows::performance_counter().unwrap_or(0),
            placement,
        ));
        unsafe {
            (self.functions.bind_buffer)(GL_PIXEL_PACK_BUFFER, self.pbos[write_index]);
            (self.functions.read_pixels)(
                x,
                y,
                self.width as i32,
                self.height as i32,
                super::GL_RGBA,
                super::GL_UNSIGNED_BYTE,
                ptr::null_mut(),
            );
        }
        self.next_pbo = (write_index + 1) % self.pbos.len();
        self.issued += 1;
        if self.issued < self.pbos.len() as u64 {
            unsafe {
                (self.functions.bind_buffer)(GL_PIXEL_PACK_BUFFER, old_pixel_pack_buffer as u32)
            };
            return Ok(None);
        }
        let read_index = self.next_pbo;
        unsafe { (self.functions.bind_buffer)(GL_PIXEL_PACK_BUFFER, self.pbos[read_index]) };
        let data = unsafe { (self.functions.map_buffer)(GL_PIXEL_PACK_BUFFER, GL_READ_ONLY) };
        if data.is_null() {
            unsafe {
                (self.functions.bind_buffer)(GL_PIXEL_PACK_BUFFER, old_pixel_pack_buffer as u32)
            };
            return Err("glMapBuffer returned null".into());
        }
        let reservation = self
            .scheduler
            .try_acquire(self.ring.slots().len(), |index| unsafe {
                polymer_producer_d3d11::KeyedMutexLease::try_acquire(
                    self.ring.slots()[index]
                        .keyed_mutex
                        .as_ref()
                        .expect("keyed ring"),
                    0,
                    1,
                )
                .ok()
                .flatten()
            });
        let Some(reservation) = reservation else {
            unsafe {
                (self.functions.unmap_buffer)(GL_PIXEL_PACK_BUFFER);
                (self.functions.bind_buffer)(GL_PIXEL_PACK_BUFFER, old_pixel_pack_buffer as u32);
            }
            return Ok(None);
        };
        let output_index = reservation.0;
        let slot = &self.ring.slots()[output_index];
        unsafe {
            self.context.UpdateSubresource(
                &slot.texture,
                0,
                None,
                data,
                self.width * 4,
                self.byte_len as u32,
            );
            (self.functions.unmap_buffer)(GL_PIXEL_PACK_BUFFER);
            (self.functions.bind_buffer)(GL_PIXEL_PACK_BUFFER, old_pixel_pack_buffer as u32);
        }
        reservation
            .1
            .finish()
            .map_err(|e| format!("IDXGIKeyedMutex::ReleaseSync: {e}"))?;
        self.scheduler.commit(output_index, self.ring.slots().len());
        let (timestamp, placement) = self.frames[read_index]
            .take()
            .expect("completed PBO has capture metadata");
        Ok(Some((output_index, timestamp, placement)))
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        unsafe { (self.functions.delete_buffers)(self.pbos.len() as i32, self.pbos.as_ptr()) }
    }
}
