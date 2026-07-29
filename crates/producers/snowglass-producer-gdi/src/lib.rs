//! Direct GDI rendering into a shared D3D11 surface.
#![allow(unsafe_code)]

use parking_lot::Mutex;
use snowglass_ipc::{LocalSocket, Message, StreamReady};
use snowglass_platform_windows::{ProcessId, duplicate_handle_into_process, performance_counter};
use snowglass_producer_d3d11::D3d11ImageRing;
use snowglass_protocol::{
    AdapterLuid, ConsumerApi, ExternalImageKind, ExternalSyncKind, FrameReady, MAX_IMAGE_SLOTS,
    PROTOCOL_VERSION, PresentationTransform, StreamId, SurfaceId,
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
    ipc: Option<LocalSocket>,
    sync_value: u64,
    frame_number: u64,
    initialized: bool,
    active_dc: Option<isize>,
    dirty: RECT,
    last_present: Instant,
}

// SAFETY: access to the immediate context and the outstanding surface DC is
// serialized by STATE. GDI painting itself remains on the calling GUI thread.
unsafe impl Send for Capture {}

#[derive(Default)]
struct State {
    capture: Option<Capture>,
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
    if state
        .capture
        .as_ref()
        .is_some_and(|capture| capture.active_dc.is_some())
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
    let recreate = state.capture.as_ref().is_none_or(|capture| {
        capture.hwnd != hwnd || capture.width != width || capture.height != height
    });
    if recreate {
        state.capture = match unsafe { Capture::new(hwnd, width, height) } {
            Ok(capture) => Some(capture),
            Err(error) => {
                log(format!("snowglass: GDI capture creation failed: {error}"));
                None
            }
        };
    }
    let capture = state.capture.as_mut()?;
    let released = capture
        .ring
        .consumer_fence()
        .is_some_and(|fence| unsafe { fence.GetCompletedValue() } >= capture.sync_value);
    if capture.sync_value != 0 && !released {
        return None;
    }
    let surface: IDXGISurface1 = capture.ring.slots()[0].texture.cast().ok()?;
    let dc = unsafe { surface.GetDC(!capture.initialized) }.ok()?;
    if !capture.initialized {
        unsafe { PatBlt(dc, 0, 0, width as i32, height as i32, BLACKNESS) };
        capture.initialized = true;
    }
    let [left, top, right, bottom] = dirty;
    if right > left && bottom > top {
        unsafe { IntersectClipRect(dc, left, top, right, bottom) };
    }
    capture.active_dc = Some(dc.0 as isize);
    capture.dirty = RECT {
        left,
        top,
        right,
        bottom,
    };
    Some(dc.0 as isize)
}

/// Completes a redirected paint and publishes the shared surface.
pub unsafe fn end_paint(hwnd: isize, dc: isize) {
    let mut state = state().lock();
    let Some(capture) = state.capture.as_mut() else {
        return;
    };
    if capture.hwnd != hwnd || capture.active_dc != Some(dc) {
        return;
    }
    unsafe { GdiFlush() };
    let surface = match capture.ring.slots()[0].texture.cast::<IDXGISurface1>() {
        Ok(surface) => surface,
        Err(error) => {
            capture.active_dc = None;
            log(format!("snowglass: GDI surface query failed: {error}"));
            return;
        }
    };
    if let Err(error) = unsafe { surface.ReleaseDC(Some(&raw const capture.dirty)) } {
        capture.active_dc = None;
        log(format!("snowglass: GDI ReleaseDC failed: {error}"));
        return;
    }
    capture.active_dc = None;
    let value = capture.sync_value.wrapping_add(1).max(1);
    let fence = capture
        .ring
        .fence()
        .expect("GDI rings carry a producer fence");
    if let Err(error) = unsafe { capture.context.Signal(fence, value) } {
        log(format!(
            "snowglass: GDI producer-fence signal failed: {error}"
        ));
        return;
    }
    capture.sync_value = value;
    capture.frame_number = capture.frame_number.wrapping_add(1);
    capture.last_present = Instant::now();
    let message = Message::FrameReady(FrameReady {
        stream_id: StreamId(hwnd as u64),
        slot: 0,
        render_width: capture.width,
        render_height: capture.height,
        capture_x: 0,
        capture_y: 0,
        capture_width: capture.width,
        capture_height: capture.height,
        presentation_width: capture.width,
        presentation_height: capture.height,
        presentation_transform: PresentationTransform::axis_aligned(
            0,
            0,
            capture.width,
            capture.height,
        ),
        frame_number: capture.frame_number,
        sync_value: value,
        timestamp_qpc: performance_counter().unwrap_or(0),
    });
    if capture
        .ipc
        .as_mut()
        .is_some_and(|ipc| ipc.send(message).is_err())
    {
        capture.ipc = None;
    }
    if capture.frame_number == 1 || capture.frame_number % 120 == 0 {
        log(format!(
            "snowglass: submitted GDI capture {}x{} (count={})",
            capture.width, capture.height, capture.frame_number
        ));
    }
}

impl Capture {
    unsafe fn new(hwnd: isize, width: u32, height: u32) -> windows::core::Result<Self> {
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
        let ipc = connect_viewer(hwnd, adapter_luid, &ring).map_err(|error| {
            Error::new(
                HRESULT(0x8000_4005u32 as i32),
                format!("viewer connection: {error}"),
            )
        })?;
        log(format!(
            "snowglass: attached GDI {width}x{height} shared DXGI surface"
        ));
        Ok(Self {
            hwnd,
            width,
            height,
            context,
            ring,
            ipc: Some(ipc),
            sync_value: 0,
            frame_number: 0,
            initialized: false,
            active_dc: None,
            dirty: RECT::default(),
            last_present: Instant::now(),
        })
    }
}

fn connect_viewer(
    hwnd: isize,
    adapter_luid: AdapterLuid,
    ring: &D3d11ImageRing,
) -> std::io::Result<LocalSocket> {
    let session = snowglass_hook::config::session()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::NotFound, error))?;
    let mut socket = LocalSocket::connect(&session)?;
    let hello = match socket.receive()? {
        Message::Hello(hello) if hello.protocol_version == PROTOCOL_VERSION => hello,
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "viewer did not begin with a compatible Hello message",
            ));
        }
    };
    socket.enable_frame_channel(hello.process_id)?;
    if hello.preferred_consumer != ConsumerApi::D3d12 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "GDI capture currently requires the D3D12 viewer",
        ));
    }
    if ring.slots().len() > MAX_IMAGE_SLOTS {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "GDI ring exceeds protocol capacity",
        ));
    }
    let viewer = ProcessId(hello.process_id);
    let mut image_handles = [0; MAX_IMAGE_SLOTS];
    image_handles[0] =
        duplicate_handle_into_process(ring.slots()[0].raw_handle() as isize, viewer)? as i64;
    let producer = duplicate_handle_into_process(
        ring.raw_fence_handle().expect("GDI producer fence") as isize,
        viewer,
    )? as i64;
    let consumer = duplicate_handle_into_process(
        ring.raw_consumer_fence_handle()
            .expect("GDI consumer fence") as isize,
        viewer,
    )? as i64;
    let desc = ring.desc();
    socket.send(Message::StreamReady(StreamReady {
        producer_process_id: std::process::id(),
        surface_id: SurfaceId(hwnd as u64),
        adapter_luid,
        source_window: hwnd as i64,
        width: desc.width,
        height: desc.height,
        format: desc.format,
        image_kind: ExternalImageKind::GdiD3d12Resource,
        sync_kind: ExternalSyncKind::D3d12Fence,
        slot_count: 1,
        image_handles,
        timeline_semaphore_handle: producer,
        consumer_semaphore_handle: consumer,
        bottom_up: false,
    }))?;
    Ok(socket)
}

fn invalid_pointer() -> Error {
    Error::from_hresult(HRESULT(0x8000_4003u32 as i32))
}

fn log(message: impl std::fmt::Display) {
    use std::io::Write;
    eprintln!("{message}");
    let Some(path) = snowglass_hook::config::gdi_log_file() else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{message}");
    }
}
