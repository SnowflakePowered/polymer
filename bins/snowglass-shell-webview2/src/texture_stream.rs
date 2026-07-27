//! Hand-rolled bindings for WebView2's experimental texture streams.
//!
//! A texture stream lets the host hand GPU textures to the page, where script
//! consumes them as a `MediaStream`. That makes a guest frame an ordinary DOM
//! node instead of a surface composited underneath the page, so CSS decides its
//! size, stacking and clipping.
//!
//! These interfaces ship only in the prerelease SDK, so `webview2-com` does not
//! declare them. The identifiers and vtable layouts below come from
//! `WebView2Experimental.h`; the order of the function pointers *is* the ABI, so
//! every slot up to the last one called has to be present even when unused.
#![allow(unsafe_code)]

use std::{
    ffi::c_void,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};
use windows::{
    Win32::Foundation::{E_NOINTERFACE, HANDLE, S_OK},
    core::{BOOL, GUID, HRESULT, IUnknown, Interface, PCWSTR, PWSTR},
};

const IID_UNKNOWN: GUID = GUID::from_u128(0x0000_0000_0000_0000_c000_0000_0000_0046);
const IID_ENVIRONMENT12: GUID = GUID::from_u128(0x96c2_7a45_f142_4873_80ad_9d0c_d899_b2b9);
const IID_START_REQUESTED_HANDLER: GUID =
    GUID::from_u128(0x62d0_9330_00a9_41bf_a9ae_55aa_ef8b_3c44);

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct EventRegistrationToken {
    value: i64,
}

#[repr(C)]
struct Environment12Vtbl {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    create_texture_stream:
        unsafe extern "system" fn(*mut c_void, PCWSTR, *mut c_void, *mut *mut c_void) -> HRESULT,
}

#[repr(C)]
struct TextureStreamVtbl {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    get_id: unsafe extern "system" fn(*mut c_void, *mut PWSTR) -> HRESULT,
    add_allowed_origin: unsafe extern "system" fn(*mut c_void, PCWSTR, BOOL) -> HRESULT,
    remove_allowed_origin: unsafe extern "system" fn(*mut c_void, PCWSTR) -> HRESULT,
    add_start_requested:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *mut EventRegistrationToken) -> HRESULT,
    remove_start_requested:
        unsafe extern "system" fn(*mut c_void, EventRegistrationToken) -> HRESULT,
    add_stopped:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *mut EventRegistrationToken) -> HRESULT,
    remove_stopped: unsafe extern "system" fn(*mut c_void, EventRegistrationToken) -> HRESULT,
    create_texture: unsafe extern "system" fn(*mut c_void, u32, u32, *mut *mut c_void) -> HRESULT,
    get_available_texture: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    close_texture: unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT,
    present_texture: unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT,
    stop: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    add_error_received:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *mut EventRegistrationToken) -> HRESULT,
    remove_error_received:
        unsafe extern "system" fn(*mut c_void, EventRegistrationToken) -> HRESULT,
    set_d3d_device: unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT,
}

#[repr(C)]
struct TextureVtbl {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    get_handle: unsafe extern "system" fn(*mut c_void, *mut HANDLE) -> HRESULT,
    get_resource: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    get_timestamp: unsafe extern "system" fn(*mut c_void, *mut u64) -> HRESULT,
    put_timestamp: unsafe extern "system" fn(*mut c_void, u64) -> HRESULT,
}

/// An owned pointer to one of the experimental interfaces.
struct ComPtr<V> {
    raw: *mut c_void,
    vtable: *const V,
}

impl<V> ComPtr<V> {
    /// # Safety
    ///
    /// `raw` must be an owned reference to an interface whose vtable layout is
    /// `V`.
    const unsafe fn from_raw(raw: *mut c_void) -> Self {
        // SAFETY: the caller guarantees the layout; the first word of any COM
        // object is its vtable pointer.
        let vtable = unsafe { *raw.cast::<*const V>() };
        Self { raw, vtable }
    }

    fn vtable(&self) -> &V {
        // SAFETY: retained for the lifetime of this value.
        unsafe { &*self.vtable }
    }
}

impl<V> Drop for ComPtr<V> {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            // SAFETY: `Release` is always the third slot of a COM vtable, and
            // this value owns one reference.
            unsafe {
                let release = *self
                    .vtable
                    .cast::<unsafe extern "system" fn(*mut c_void) -> u32>()
                    .add(2);
                release(self.raw);
            }
        }
    }
}

// SAFETY: the stream is only touched from the thread that owns the webview.
unsafe impl<V> Send for ComPtr<V> {}

/// A texture stream the page can consume, plus the textures cycled through it.
pub struct TextureStream {
    stream: ComPtr<TextureStreamVtbl>,
    started: std::sync::Arc<AtomicBool>,
}

impl TextureStream {
    /// Creates a stream named `id` on `environment`, backed by `device`.
    ///
    /// `device` is the D3D device WebView2 allocates its shared textures on, so
    /// it has to be one this process can also write through.
    pub fn create(
        environment: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Environment,
        id: &str,
        device: &IUnknown,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut queried: *mut c_void = std::ptr::null_mut();
        // SAFETY: `environment` is live and `queried` is a live out-parameter.
        unsafe { environment.query(&IID_ENVIRONMENT12, &mut queried) }.ok()?;
        if queried.is_null() {
            return Err("this runtime has no experimental environment".into());
        }
        // SAFETY: the query returned an owned reference with this layout.
        let environment12 = unsafe { ComPtr::<Environment12Vtbl>::from_raw(queried) };

        let wide = id.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        let mut stream: *mut c_void = std::ptr::null_mut();
        // SAFETY: all arguments outlive the call.
        unsafe {
            (environment12.vtable().create_texture_stream)(
                environment12.raw,
                PCWSTR(wide.as_ptr()),
                device.as_raw(),
                &mut stream,
            )
        }
        .ok()?;
        if stream.is_null() {
            return Err("CreateTextureStream produced no stream".into());
        }
        // SAFETY: an owned reference with the texture stream layout.
        let stream = unsafe { ComPtr::<TextureStreamVtbl>::from_raw(stream) };
        Ok(Self {
            stream,
            started: std::sync::Arc::new(AtomicBool::new(false)),
        })
    }

    /// Permits `origin` to request this stream, and to send textures back.
    pub fn allow_origin(&self, origin: &str) -> Result<(), Box<dyn std::error::Error>> {
        let wide = origin.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
        // SAFETY: the string outlives the call.
        unsafe {
            (self.stream.vtable().add_allowed_origin)(
                self.stream.raw,
                PCWSTR(wide.as_ptr()),
                BOOL(1),
            )
        }
        .ok()?;
        Ok(())
    }

    /// Registers for the page's first request for this stream. Presenting is
    /// only meaningful once that has fired.
    pub fn on_start_requested(&self) -> Result<(), Box<dyn std::error::Error>> {
        let handler = StartRequestedHandler::new(std::sync::Arc::clone(&self.started));
        let mut token = EventRegistrationToken::default();
        // SAFETY: the handler is reference counted and released by WebView2.
        unsafe { (self.stream.vtable().add_start_requested)(self.stream.raw, handler, &mut token) }
            .ok()?;
        Ok(())
    }

    /// Whether the page has asked for this stream yet.
    pub fn started(&self) -> bool {
        self.started.load(Ordering::Acquire)
    }

    /// Allocates a texture WebView2 will recycle through the stream.
    pub fn create_texture(
        &self,
        width: u32,
        height: u32,
    ) -> Result<Texture, Box<dyn std::error::Error>> {
        let mut texture: *mut c_void = std::ptr::null_mut();
        // SAFETY: `texture` is a live out-parameter.
        unsafe {
            (self.stream.vtable().create_texture)(self.stream.raw, width, height, &mut texture)
        }
        .ok()?;
        if texture.is_null() {
            return Err("CreateTexture produced no texture".into());
        }
        // SAFETY: an owned reference with the texture layout.
        Ok(Texture(unsafe { ComPtr::from_raw(texture) }))
    }

    /// Returns a texture whose previous frame the renderer has finished with,
    /// or `None` when they are all still in flight.
    pub fn available_texture(&self) -> Option<Texture> {
        let mut texture: *mut c_void = std::ptr::null_mut();
        // SAFETY: `texture` is a live out-parameter.
        let result =
            unsafe { (self.stream.vtable().get_available_texture)(self.stream.raw, &mut texture) };
        (result.is_ok() && !texture.is_null())
            // SAFETY: an owned reference with the texture layout.
            .then(|| Texture(unsafe { ComPtr::from_raw(texture) }))
    }

    /// Hands a written texture to the page as the stream's next frame.
    pub fn present(&self, texture: &Texture) -> Result<(), Box<dyn std::error::Error>> {
        // SAFETY: the texture came from this stream and is not closed.
        unsafe { (self.stream.vtable().present_texture)(self.stream.raw, texture.0.raw) }.ok()?;
        Ok(())
    }
}

pub struct Texture(ComPtr<TextureVtbl>);

impl Texture {
    /// The underlying GPU resource, to be written before presenting.
    pub fn resource(&self) -> Option<IUnknown> {
        let mut resource: *mut c_void = std::ptr::null_mut();
        // SAFETY: `resource` is a live out-parameter.
        let result = unsafe { (self.0.vtable().get_resource)(self.0.raw, &mut resource) };
        (result.is_ok() && !resource.is_null())
            // SAFETY: the call returned an owned reference.
            .then(|| unsafe { IUnknown::from_raw(resource) })
    }

    pub fn set_timestamp(&self, timestamp: u64) {
        // SAFETY: a plain scalar setter on a live texture.
        let _ = unsafe { (self.0.vtable().put_timestamp)(self.0.raw, timestamp) };
    }
}

/// Owns the stream, the device its textures live on, and the textures cycled
/// through it.
pub struct Publisher {
    stream: TextureStream,
    device: windows::Win32::Graphics::Direct3D11::ID3D11Device,
    context: windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    textures: Vec<Texture>,
    extent: (u32, u32),
    frames: u64,
}

impl Publisher {
    /// Creates a stream on `environment` backed by a fresh D3D11 device on
    /// `adapter`, and allows `origin` to request it.
    pub fn start(
        environment: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Environment,
        adapter: &windows::Win32::Graphics::Dxgi::IDXGIAdapter,
        id: &str,
        origin: &str,
        width: u32,
        height: u32,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        use windows::Win32::Graphics::{
            Direct3D::D3D_DRIVER_TYPE_UNKNOWN,
            Direct3D11::{D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice},
        };

        let mut device = None;
        let mut context = None;
        // SAFETY: the out-parameters are live locals and the adapter outlives
        // the call.
        unsafe {
            D3D11CreateDevice(
                adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                windows::Win32::Foundation::HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )?;
        }
        let device = device.ok_or("D3D11CreateDevice returned no device")?;
        let context = context.ok_or("D3D11CreateDevice returned no context")?;

        let unknown: IUnknown = device.cast()?;
        let stream = TextureStream::create(environment, id, &unknown)?;
        stream.allow_origin(origin)?;
        stream.on_start_requested()?;

        // Three in flight matches the producer rings elsewhere: one being
        // written, one queued, one held by the renderer.
        let textures = (0..3)
            .map(|_| stream.create_texture(width, height))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            stream,
            device,
            context,
            textures,
            extent: (width, height),
            frames: 0,
        })
    }

    pub fn started(&self) -> bool {
        self.stream.started()
    }

    pub const fn frames(&self) -> u64 {
        self.frames
    }

    /// Copies `source` into a free texture and presents it as the next frame.
    /// `source` must be openable by this publisher's device.
    ///
    /// Returns false when every texture is still in flight, which is
    /// backpressure rather than an error.
    pub fn publish(
        &mut self,
        source: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        timestamp: u64,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let Some(texture) = self.stream.available_texture() else {
            return Ok(false);
        };
        let Some(resource) = texture.resource() else {
            return Err("presented texture exposed no resource".into());
        };
        let destination: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D = resource.cast()?;
        // SAFETY: both textures live on this device and have matching extents.
        unsafe {
            self.context.CopyResource(&destination, source);
            self.context.Flush();
        }
        texture.set_timestamp(timestamp);
        self.stream.present(&texture)?;
        self.frames += 1;
        Ok(true)
    }

    /// Fills a free texture with a flat colour and presents it. Used to prove
    /// the stream reaches the page before any guest frame is wired through it.
    pub fn publish_test_pattern(
        &mut self,
        timestamp: u64,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_RENDER_TARGET_VIEW_DESC, D3D11_RTV_DIMENSION_TEXTURE2D, ID3D11Texture2D,
        };

        let Some(texture) = self.stream.available_texture() else {
            return Ok(false);
        };
        let Some(resource) = texture.resource() else {
            return Err("presented texture exposed no resource".into());
        };
        let destination: ID3D11Texture2D = resource.cast()?;
        // A colour that walks each frame, so a still image on screen is
        // obviously distinguishable from a live stream.
        let shade = (self.frames % 256) as f32 / 255.0;
        let mut view = None;
        // SAFETY: the descriptor and out-parameter are live locals.
        unsafe {
            self.device.CreateRenderTargetView(
                &destination,
                Some(&D3D11_RENDER_TARGET_VIEW_DESC {
                    Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
                    ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2D,
                    ..Default::default()
                }),
                Some(&mut view),
            )?;
            if let Some(view) = view {
                self.context
                    .ClearRenderTargetView(&view, &[shade, 0.25, 1.0 - shade, 1.0]);
            }
            self.context.Flush();
        }
        texture.set_timestamp(timestamp);
        self.stream.present(&texture)?;
        self.frames += 1;
        Ok(true)
    }

    pub const fn extent(&self) -> (u32, u32) {
        self.extent
    }
}

/// A COM object implementing the start-requested callback.
#[repr(C)]
struct StartRequestedHandler {
    vtable: *const StartRequestedHandlerVtbl,
    references: AtomicU32,
    started: std::sync::Arc<AtomicBool>,
}

#[repr(C)]
struct StartRequestedHandlerVtbl {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    invoke: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> HRESULT,
}

impl StartRequestedHandler {
    fn new(started: std::sync::Arc<AtomicBool>) -> *mut c_void {
        static VTABLE: StartRequestedHandlerVtbl = StartRequestedHandlerVtbl {
            query_interface,
            add_ref,
            release,
            invoke,
        };
        let handler = Box::new(Self {
            vtable: &raw const VTABLE,
            references: AtomicU32::new(1),
            started,
        });
        Box::into_raw(handler).cast::<c_void>()
    }
}

unsafe extern "system" fn query_interface(
    this: *mut c_void,
    interface_id: *const GUID,
    output: *mut *mut c_void,
) -> HRESULT {
    if output.is_null() {
        return E_NOINTERFACE;
    }
    // SAFETY: `interface_id` and `output` are live for the call.
    unsafe {
        let requested = *interface_id;
        if requested == IID_UNKNOWN || requested == IID_START_REQUESTED_HANDLER {
            *output = this;
            add_ref(this);
            return S_OK;
        }
        *output = std::ptr::null_mut();
    }
    E_NOINTERFACE
}

unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
    // SAFETY: `this` is a live handler allocated by `new`.
    let handler = unsafe { &*this.cast::<StartRequestedHandler>() };
    handler.references.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn release(this: *mut c_void) -> u32 {
    // SAFETY: as above; the allocation is reclaimed on the last release.
    let remaining = {
        let handler = unsafe { &*this.cast::<StartRequestedHandler>() };
        handler.references.fetch_sub(1, Ordering::AcqRel) - 1
    };
    if remaining == 0 {
        drop(unsafe { Box::from_raw(this.cast::<StartRequestedHandler>()) });
    }
    remaining
}

unsafe extern "system" fn invoke(
    this: *mut c_void,
    _sender: *mut c_void,
    _args: *mut c_void,
) -> HRESULT {
    // SAFETY: `this` is a live handler for the duration of the callback.
    let handler = unsafe { &*this.cast::<StartRequestedHandler>() };
    handler.started.store(true, Ordering::Release);
    eprintln!("snowglass: the page requested the texture stream");
    S_OK
}
