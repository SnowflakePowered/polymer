mod identity;
mod presentation;
mod settings;
mod vtable;

use dashmap::DashMap;
use parking_lot::Mutex;
use std::{
    ffi::c_void,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};
use windows::{
    Win32::{
        Foundation::*,
        Graphics::Dxgi::{Common::*, *},
    },
    core::{Error, GUID, HRESULT, IUnknown, Interface},
};

/// Native frontends implement this on their producer-owned controller owner.
/// Calls must release map/state guards before invoking drivers or IPC.
pub trait VirtualDxgiDriver: Send + Sync + 'static {
    fn buffer(&self, index: u32) -> windows::core::Result<IUnknown>;
    fn present(&self, interval: u32, flags: DXGI_PRESENT) -> HRESULT;
    fn current_index(&self) -> u32;
    fn set_maximum_latency(&self, latency: u32) -> windows::core::Result<()>;
    fn maximum_latency(&self) -> u32;
    /// Caller-owned duplicate, matching DXGI's CloseHandle contract.
    fn latency_handle(&self) -> HANDLE;
}

enum PrivateData {
    Bytes(Vec<u8>),
    Interface(IUnknown),
}

/// Typed native vtables preserve the entire advertised IDXGISwapChain3 ABI.
/// All inherited interfaces share this pointer and canonical IUnknown identity.
#[repr(C)]
pub struct VirtualDxgiSwapchain {
    vtable: &'static IDXGISwapChain3_Vtbl,
    references: AtomicU32,
    parent: IUnknown,
    device: IUnknown,
    window: HWND,
    description: DXGI_SWAP_CHAIN_DESC1,
    driver: Arc<dyn VirtualDxgiDriver>,
    private: DashMap<GUID, Arc<PrivateData>>,
    background: Mutex<DXGI_RGBA>,
    presents: AtomicU32,
}

impl VirtualDxgiSwapchain {
    pub fn create(
        parent: IUnknown,
        device: IUnknown,
        window: HWND,
        description: DXGI_SWAP_CHAIN_DESC1,
        driver: Arc<dyn VirtualDxgiDriver>,
    ) -> windows::core::Result<IDXGISwapChain3> {
        Self::validate(window, &description)?;
        let object = Box::new(Self {
            vtable: &vtable::VTABLE,
            references: AtomicU32::new(1),
            parent,
            device,
            window,
            description,
            driver,
            private: DashMap::new(),
            background: Mutex::new(DXGI_RGBA {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            }),
            presents: AtomicU32::new(0),
        });
        Ok(unsafe { IDXGISwapChain3::from_raw(Box::into_raw(object).cast()) })
    }
    pub fn validate(window: HWND, desc: &DXGI_SWAP_CHAIN_DESC1) -> windows::core::Result<()> {
        if window.0.is_null() || desc.Width == 0 || desc.Height == 0 {
            return Err(Error::new(
                DXGI_ERROR_INVALID_CALL,
                "virtual DXGI requires a fixed extent and source HWND",
            ));
        }
        if desc.Stereo.as_bool() || desc.SampleDesc.Count != 1 || desc.SampleDesc.Quality != 0 {
            return Err(Error::new(
                DXGI_ERROR_UNSUPPORTED,
                "virtual DXGI profile is mono and single-sample",
            ));
        }
        if !(2..=4).contains(&desc.BufferCount) || desc.SwapEffect != DXGI_SWAP_EFFECT_FLIP_DISCARD
        {
            return Err(Error::new(
                DXGI_ERROR_UNSUPPORTED,
                "virtual DXGI profile requires flip-discard with two to four buffers",
            ));
        }
        if desc.Scaling != DXGI_SCALING_STRETCH
            || !matches!(
                desc.AlphaMode,
                DXGI_ALPHA_MODE_IGNORE | DXGI_ALPHA_MODE_UNSPECIFIED
            )
        {
            return Err(Error::new(
                DXGI_ERROR_UNSUPPORTED,
                "virtual HWND profile requires stretch scaling and opaque alpha",
            ));
        }
        let allowed = (DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING
            | DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT)
            .0 as u32;
        if desc.Flags & !allowed != 0 {
            return Err(Error::new(
                DXGI_ERROR_UNSUPPORTED,
                "unsupported virtual DXGI creation flags",
            ));
        }
        Ok(())
    }
    unsafe fn from_this<'a>(this: *mut c_void) -> &'a Self {
        unsafe { &*this.cast::<Self>() }
    }
    fn guarded(action: impl FnOnce() -> windows::core::Result<()>) -> HRESULT {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(action)).map_or(E_FAIL, |result| {
            result.map_or_else(|error| error.code(), |()| S_OK)
        })
    }
}
