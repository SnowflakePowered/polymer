//! Direct GDI rendering into a shared D3D11 surface.
#![allow(unsafe_code)]

mod paint;
use parking_lot::Mutex;
use polymer_platform_windows::performance_counter;
use polymer_presentation::producer::ExportDescriptor;
use polymer_presentation::producer::FramePublisher;
use polymer_presentation::producer::ProducerSession;
use polymer_producer_d3d11::D3d11ImageRing;
use polymer_protocol::{
    AdapterLuid, ConsumerApi, ExternalImageKind, ExternalSyncKind, PresentationTransform, StreamId,
    SurfaceId,
};
use std::{
    sync::OnceLock,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{HMODULE, RECT},
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0},
            Direct3D11::{
                D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
                D3D11_USAGE_DEFAULT, D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext4,
            },
            Dxgi::{
                Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
                CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, IDXGIAdapter, IDXGIFactory4,
                IDXGISurface1,
            },
            Gdi::HDC,
        },
    },
    core::{Error, HRESULT, Interface},
};

pub const BACKEND_NAME: &str = "gdi";

#[link(name = "gdi32")]
unsafe extern "system" {
    fn GdiFlush() -> i32;
    fn IntersectClipRect(hdc: HDC, left: i32, top: i32, right: i32, bottom: i32) -> i32;
    fn PatBlt(hdc: HDC, x: i32, y: i32, width: i32, height: i32, rop: u32) -> i32;
}

const BLACKNESS: u32 = 0x0000_0042;

struct Capture {
    hwnd: isize,
    width: u32,
    height: u32,
    context: ID3D11DeviceContext4,
    ring: D3d11ImageRing,
    ipc: Option<ProducerSession>,
    sync_value: u64,
    initialized: bool,
    active_paint: Option<paint::PaintReservation>,
    pending_release: Option<paint::PaintReservation>,
    scheduler: polymer_presentation::scheduling::SlotScheduler,
    submitted_value: u64,
    pending_signal: Option<u64>,
    last_present: Instant,
}

// SAFETY: access to the immediate context and the outstanding surface DC is
// serialized by STATE. GDI painting itself remains on the calling GUI thread.
unsafe impl Send for Capture {}

#[derive(Default)]
struct State {
    capture: polymer_presentation::producer::CaptureSession<Capture, (isize, u32, u32)>,
    destroyed: bool,
}

impl State {
    fn retire_destroyed(&mut self) {
        if self.destroyed
            && self.capture.as_ref().is_none_or(|capture| {
                capture.active_paint.is_none() && capture.pending_release.is_none()
            })
        {
            self.capture.invalidate();
            self.destroyed = false;
        }
    }

    fn window_destroyed(&mut self, hwnd: isize) {
        let matches = self
            .capture
            .as_ref()
            .is_some_and(|capture| capture.hwnd == hwnd)
            || self
                .capture
                .configuration()
                .is_some_and(|configuration| configuration.0 == hwnd);
        if !matches {
            return;
        }
        self.destroyed = true;
        if let Some(capture) = self.capture.as_mut() {
            capture.ipc.take();
            if capture.active_paint.is_none() {
                let _ = capture.recover_paint();
            }
        }
        self.retire_destroyed();
    }
}

/// Notify only after successful destruction of the source window.
pub fn window_destroyed(hwnd: isize) {
    state().lock().window_destroyed(hwnd);
}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();

fn state() -> &'static Mutex<State> {
    STATE.get_or_init(Default::default)
}

/// Replaces a top-level window paint DC with a shared DXGI-surface DC.
/// Returns `None` when capture is unavailable; the caller must retain the real DC.
pub unsafe fn begin_paint(hwnd: isize, width: u32, height: u32, dirty: [i32; 4]) -> Option<isize> {
    if hwnd == 0 || width == 0 || height == 0 || width > 8192 || height > 8192 {
        return None;
    }
    let mut state = state().lock();
    if let Some(capture) = state.capture.as_mut() {
        if capture.recover_paint() != Ok(true) {
            return None;
        }
    }
    state.retire_destroyed();
    if state
        .capture
        .as_ref()
        .is_some_and(|capture| capture.active_paint.is_some())
    {
        return None;
    }
    if state.capture.as_ref().is_some_and(|capture| {
        capture.hwnd != hwnd
            && u64::from(capture.width) * u64::from(capture.height)
                >= u64::from(width) * u64::from(height)
            && capture.last_present.elapsed() < Duration::from_millis(500)
    }) {
        return None;
    }
    state.capture.configure((hwnd, width, height));
    let setup = CaptureSetup {
        hwnd,
        width,
        height,
    };
    if let Err(error) = unsafe { state.capture.initialize(Instant::now(), &setup) } {
        tracing::debug!("polymer: GDI capture creation failed: {error}");
    }
    match unsafe { state.capture.capture(paint::BeginPaint { dirty }) } {
        Ok(dc) => dc,
        Err(_) => None,
    }
}

/// Completes a redirected paint and publishes the shared surface.
pub unsafe fn end_paint(hwnd: isize, dc: isize) {
    let mut state = state().lock();
    let _ = unsafe { state.capture.capture(paint::EndPaint { hwnd, dc }) };
    state.retire_destroyed();
}

impl Capture {
    unsafe fn new(hwnd: isize, width: u32, height: u32) -> windows::core::Result<Option<Self>> {
        let name = polymer_hook::config::session()
            .map_err(|error| Error::new(HRESULT(0x8000_4005u32 as i32), error.to_string()))?;
        let Some(socket) = ProducerSession::setup_result(ProducerSession::connect(&name))
            .map_err(|error| Error::new(HRESULT(0x8000_4005u32 as i32), error.to_string()))?
        else {
            return Ok(None);
        };
        if socket.preferred_consumer() != ConsumerApi::D3d12 {
            tracing::debug!("polymer: GDI requires a D3D12 consumer");
            return Ok(None);
        }

        let factory = unsafe { CreateDXGIFactory2::<IDXGIFactory4>(DXGI_CREATE_FACTORY_FLAGS(0)) }?;
        let adapter = unsafe { factory.EnumAdapters1(0) }?;
        let adapter_base: IDXGIAdapter = adapter.cast()?;
        let mut device: Option<ID3D11Device> = None;
        let mut immediate = None;
        unsafe {
            D3D11CreateDevice(
                &adapter_base,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut immediate),
            )?;
        }
        let device = device.ok_or_else(invalid_pointer)?;
        let context: ID3D11DeviceContext4 = immediate.ok_or_else(invalid_pointer)?.cast()?;
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE).0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let ring = unsafe { D3d11ImageRing::new_gdi(&device, &desc, 1) }?;
        let adapter_desc = unsafe { adapter.GetDesc1() }?;
        let adapter_luid = AdapterLuid {
            low_part: adapter_desc.AdapterLuid.LowPart,
            high_part: adapter_desc.AdapterLuid.HighPart,
        };
        let Some(ipc) =
            ProducerSession::setup_result(connect_viewer(hwnd, adapter_luid, &ring, socket))
                .map_err(|error| {
                    Error::new(
                        HRESULT(0x8000_4005u32 as i32),
                        format!("viewer connection: {error}"),
                    )
                })?
        else {
            return Ok(None);
        };
        tracing::debug!("polymer: attached GDI {width}x{height} shared DXGI surface");
        Ok(Some(Self {
            hwnd,
            width,
            height,
            context,
            ring,
            ipc: Some(ipc),
            sync_value: 0,
            initialized: false,
            active_paint: None,
            pending_release: None,
            scheduler: Default::default(),
            submitted_value: 0,
            pending_signal: None,
            last_present: Instant::now(),
        }))
    }
}

fn connect_viewer(
    hwnd: isize,
    adapter_luid: AdapterLuid,
    ring: &D3d11ImageRing,
    mut socket: ProducerSession,
) -> std::io::Result<ProducerSession> {
    let image_handles = ring
        .slots()
        .iter()
        .map(|slot| slot.raw_handle() as isize as i64)
        .collect::<Vec<_>>();
    let producer = (ring.raw_fence_handle().expect("GDI producer fence") as isize) as i64;
    let consumer = (ring
        .raw_consumer_fence_handle()
        .expect("GDI consumer fence") as isize) as i64;
    let desc = ring.desc();
    socket.publish(ExportDescriptor {
        surface_id: SurfaceId(hwnd as u64),
        adapter_luid,
        source_window: hwnd as i64,
        width: desc.width,
        height: desc.height,
        format: desc.format,
        image_kind: ExternalImageKind::GdiD3d12Resource,
        sync_kind: ExternalSyncKind::D3d12Fence,
        image_handles: &image_handles,
        timeline_semaphore_handle: producer,
        consumer_semaphore_handle: consumer,
        bottom_up: false,
    })?;
    Ok(socket)
}

fn invalid_pointer() -> Error {
    Error::from_hresult(HRESULT(0x8000_4003u32 as i32))
}

impl polymer_presentation::producer::CaptureBackend for Capture {
    fn connected(&self) -> bool {
        // Preserve the owning thread's DC until EndPaint or release recovery;
        // that boundary observes publication failure before parking the stream.
        self.active_paint.is_some()
            || self.pending_release.is_some()
            || self.ipc.as_ref().is_some_and(ProducerSession::connected)
    }
}

struct CaptureSetup {
    hwnd: isize,
    width: u32,
    height: u32,
}
impl polymer_presentation::producer::CaptureInitializer for CaptureSetup {
    type Backend = Capture;
    type Error = windows::core::Error;
    unsafe fn initialize(&self) -> Result<Option<Capture>, Self::Error> {
        unsafe { Capture::new(self.hwnd, self.width, self.height) }
    }
}

#[cfg(test)]
mod tests;
