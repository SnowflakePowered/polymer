use crate::{
    hooks,
    vtable::{
        FACTORY_VTABLE_LEN, FACTORY1_VTABLE_LEN, FACTORY2_VTABLE_LEN, FACTORY3_VTABLE_LEN,
        FACTORY4_VTABLE_LEN, FACTORY5_VTABLE_LEN, FACTORY6_VTABLE_LEN, FACTORY7_VTABLE_LEN,
        FactoryVtable, SwapchainVtable,
    },
};
use snowglass_hook::HookChain;
use std::{
    collections::HashMap,
    ffi::c_void,
    sync::{Arc, OnceLock, RwLock},
};
use windows::{
    Win32::Graphics::Dxgi::{
        DXGI_ERROR_INVALID_CALL, IDXGIFactory, IDXGIFactory1, IDXGIFactory2, IDXGIFactory3,
        IDXGIFactory4, IDXGIFactory5, IDXGIFactory6, IDXGIFactory7, IDXGISwapChain,
        IDXGISwapChain1, IDXGISwapChain2, IDXGISwapChain3, IDXGISwapChain4,
    },
    core::{GUID, Interface},
};

#[derive(Clone, Copy, Default)]
pub struct SwapchainCallbacks {
    pub before_present: Option<unsafe fn(*mut c_void)>,
    pub before_resize: Option<unsafe fn(*mut c_void)>,
    pub after_resize: Option<unsafe fn(*mut c_void, i32)>,
    pub destroyed: Option<unsafe fn(*mut c_void)>,
}

static CALLBACKS: OnceLock<SwapchainCallbacks> = OnceLock::new();
static SWAPCHAINS: OnceLock<RwLock<HashMap<usize, Arc<SwapchainVtable>>>> = OnceLock::new();
static FACTORIES: OnceLock<RwLock<HashMap<usize, Arc<FactoryVtable>>>> = OnceLock::new();

fn swapchains() -> &'static RwLock<HashMap<usize, Arc<SwapchainVtable>>> {
    SWAPCHAINS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn factories() -> &'static RwLock<HashMap<usize, Arc<FactoryVtable>>> {
    FACTORIES.get_or_init(|| RwLock::new(HashMap::new()))
}

pub fn set_callbacks(callbacks: SwapchainCallbacks) -> Result<(), SwapchainCallbacks> {
    CALLBACKS.set(callbacks)
}

pub unsafe fn install_swapchain(swapchain: *mut c_void) -> bool {
    unsafe { install_swapchain_kind(swapchain, false) }
}

pub unsafe fn install_swapchain1(swapchain: *mut c_void) -> bool {
    unsafe { install_swapchain_kind(swapchain, true) }
}

unsafe fn install_swapchain_kind(swapchain: *mut c_void, swapchain1: bool) -> bool {
    if swapchain.is_null() {
        return false;
    }
    let key = swapchain as usize;
    let mut states = swapchains()
        .write()
        .unwrap_or_else(|error| error.into_inner());
    let vtable = if let Some(existing) = states.get(&key) {
        let current = unsafe { *swapchain.cast::<*const usize>() };
        if current == existing.patched.as_ptr() {
            if !swapchain1 || existing.present1.is_some() {
                return true;
            }
            unsafe { SwapchainVtable::clone_from(existing.original as *const usize, true) }
        } else if swapchain1 {
            unsafe { SwapchainVtable::clone_and_patch_swapchain1(swapchain) }
        } else {
            unsafe { SwapchainVtable::clone_and_patch(swapchain) }
        }
    } else if swapchain1 {
        unsafe { SwapchainVtable::clone_and_patch_swapchain1(swapchain) }
    } else {
        unsafe { SwapchainVtable::clone_and_patch(swapchain) }
    };
    let Some(vtable) = vtable else {
        return false;
    };
    let state = Arc::new(vtable);
    unsafe { *swapchain.cast::<*const usize>() = state.patched.as_ptr() };
    states.insert(key, state);
    true
}

pub unsafe fn install_factory(factory: *mut c_void) -> bool {
    unsafe { install_factory_len(factory, FACTORY_VTABLE_LEN) }
}

/// Installs per-object hooks for every factory interface supported by a DXGI
/// factory and seeds the process-wide Factory2+ method detour.
///
/// # Safety
///
/// `factory` must be null or point to a live `IDXGIFactory` interface for the
/// duration of this call.
pub unsafe fn install_factory_interfaces(factory: *mut c_void) -> bool {
    let Some(factory) = (unsafe { IDXGIFactory::from_raw_borrowed(&factory) }) else {
        return false;
    };
    let mut installed = false;
    macro_rules! install_interface {
        ($interface:ty) => {
            if let Ok(interface) = factory.cast::<$interface>() {
                installed |= unsafe {
                    install_factory_interface(
                        interface.as_raw(),
                        (&<$interface>::IID as *const GUID).cast(),
                    )
                };
            }
        };
    }
    install_interface!(IDXGIFactory7);
    install_interface!(IDXGIFactory6);
    install_interface!(IDXGIFactory5);
    install_interface!(IDXGIFactory4);
    install_interface!(IDXGIFactory3);
    install_interface!(IDXGIFactory2);
    install_interface!(IDXGIFactory1);
    installed | unsafe { install_factory(factory.as_raw()) }
}

pub unsafe fn install_factory_interface(factory: *mut c_void, interface_id: *const c_void) -> bool {
    if interface_id.is_null() {
        return false;
    }
    let interface_id = unsafe { &*interface_id.cast::<GUID>() };
    let Some(length) = factory_vtable_len(interface_id) else {
        return false;
    };
    unsafe { install_factory_len(factory, length) }
}

unsafe fn install_factory_len(factory: *mut c_void, length: usize) -> bool {
    if factory.is_null() {
        return false;
    }
    if length >= FACTORY2_VTABLE_LEN {
        unsafe { crate::detour::install_create_swapchain_for_hwnd(factory) };
    }
    let key = factory as usize;
    let mut states = factories()
        .write()
        .unwrap_or_else(|error| error.into_inner());
    let vtable = if let Some(existing) = states.get(&key) {
        let current = unsafe { *factory.cast::<*const usize>() };
        if current == existing.patched.as_ptr() {
            if existing.patched.len() >= length {
                return true;
            }
            unsafe { FactoryVtable::clone_from(existing.original as *const usize, length) }
        } else {
            unsafe { FactoryVtable::clone_and_patch_len(factory, length) }
        }
    } else {
        unsafe { FactoryVtable::clone_and_patch_len(factory, length) }
    };
    let Some(vtable) = vtable else {
        return false;
    };
    let state = Arc::new(vtable);
    unsafe { *factory.cast::<*const usize>() = state.patched.as_ptr() };
    states.insert(key, state);
    true
}

fn factory_vtable_len(interface_id: &GUID) -> Option<usize> {
    [
        (&IDXGIFactory::IID, FACTORY_VTABLE_LEN),
        (&IDXGIFactory1::IID, FACTORY1_VTABLE_LEN),
        (&IDXGIFactory2::IID, FACTORY2_VTABLE_LEN),
        (&IDXGIFactory3::IID, FACTORY3_VTABLE_LEN),
        (&IDXGIFactory4::IID, FACTORY4_VTABLE_LEN),
        (&IDXGIFactory5::IID, FACTORY5_VTABLE_LEN),
        (&IDXGIFactory6::IID, FACTORY6_VTABLE_LEN),
        (&IDXGIFactory7::IID, FACTORY7_VTABLE_LEN),
    ]
    .into_iter()
    .find_map(|(candidate, length)| (interface_id == candidate).then_some(length))
}

pub(crate) fn swapchain_method<T: Copy>(
    swapchain: *mut c_void,
    select: impl FnOnce(&SwapchainVtable) -> T,
) -> Option<T> {
    let states = swapchains()
        .read()
        .unwrap_or_else(|error| error.into_inner());
    states.get(&(swapchain as usize)).map(|state| select(state))
}

pub(crate) fn factory_method<T: Copy>(
    factory: *mut c_void,
    select: impl FnOnce(&FactoryVtable) -> Option<T>,
) -> Option<T> {
    let states = factories()
        .read()
        .unwrap_or_else(|error| error.into_inner());
    states
        .get(&(factory as usize))
        .and_then(|state| select(state))
}

pub(crate) fn query_method(
    factory_or_swapchain: *mut c_void,
) -> Option<crate::vtable::QueryInterfaceFn> {
    if let Some(method) = swapchain_method(factory_or_swapchain, |vtable| vtable.query_interface) {
        return Some(method);
    }
    factory_method(factory_or_swapchain, |vtable| Some(vtable.query_interface))
}

pub(crate) unsafe fn factory_created_swapchain(swapchain: *mut *mut c_void, result: i32) {
    crate::dxgi_log(format!(
        "snowglass: DXGI swapchain creation returned {result:#x}"
    ));
    if result < 0 || swapchain.is_null() {
        return;
    }
    let swapchain = unsafe { *swapchain };
    if swapchain.is_null() {
        return;
    }
    let swapchain1 = unsafe { IDXGISwapChain::from_raw_borrowed(&swapchain) }
        .and_then(|swapchain| swapchain.cast::<IDXGISwapChain1>().ok());
    unsafe { install_swapchain(swapchain) };
    if let Some(swapchain1) = swapchain1 {
        unsafe { install_swapchain1(swapchain1.as_raw()) };
    }
}

pub(crate) unsafe fn query_interface(
    this: *mut c_void,
    interface_id: *const c_void,
    output: *mut *mut c_void,
    mut next: hooks::QueryInterfaceContext<'_>,
) -> i32 {
    let Some(continuation) = next.fp_next().copied() else {
        return 0x8000_4002u32 as i32;
    };
    let result = unsafe { continuation(this, interface_id, output, next) };
    if result >= 0 && !interface_id.is_null() && !output.is_null() {
        let interface_id = unsafe { &*interface_id.cast::<GUID>() };
        let swapchain = unsafe { *output };
        if !swapchain.is_null() {
            if interface_id == &IDXGISwapChain::IID {
                unsafe { install_swapchain(swapchain) };
            } else if [
                &IDXGISwapChain1::IID,
                &IDXGISwapChain2::IID,
                &IDXGISwapChain3::IID,
                &IDXGISwapChain4::IID,
            ]
            .into_iter()
            .any(|candidate| interface_id == candidate)
            {
                unsafe { install_swapchain1(swapchain) };
            } else {
                unsafe {
                    install_factory_interface(
                        swapchain,
                        (interface_id as *const GUID).cast::<c_void>(),
                    )
                };
            }
        }
    }
    result
}

pub(crate) unsafe fn present(
    this: *mut c_void,
    sync_interval: u32,
    flags: u32,
    mut next: hooks::PresentContext<'_>,
) -> i32 {
    if let Some(callback) = CALLBACKS
        .get()
        .and_then(|callbacks| callbacks.before_present)
    {
        unsafe { callback(this) };
    }
    let Some(continuation) = next.fp_next().copied() else {
        return DXGI_ERROR_INVALID_CALL.0;
    };
    unsafe { continuation(this, sync_interval, flags, next) }
}

pub(crate) unsafe fn present1(
    this: *mut c_void,
    sync_interval: u32,
    flags: u32,
    parameters: *const c_void,
    mut next: hooks::Present1Context<'_>,
) -> i32 {
    if let Some(callback) = CALLBACKS
        .get()
        .and_then(|callbacks| callbacks.before_present)
    {
        unsafe { callback(this) };
    }
    let Some(continuation) = next.fp_next().copied() else {
        return DXGI_ERROR_INVALID_CALL.0;
    };
    unsafe { continuation(this, sync_interval, flags, parameters, next) }
}

pub(crate) unsafe fn resize_buffers(
    this: *mut c_void,
    buffer_count: u32,
    width: u32,
    height: u32,
    format: u32,
    flags: u32,
    mut next: hooks::ResizeBuffersContext<'_>,
) -> i32 {
    if let Some(callback) = CALLBACKS
        .get()
        .and_then(|callbacks| callbacks.before_resize)
    {
        unsafe { callback(this) };
    }
    let Some(continuation) = next.fp_next().copied() else {
        return DXGI_ERROR_INVALID_CALL.0;
    };
    let result = unsafe { continuation(this, buffer_count, width, height, format, flags, next) };
    if let Some(callback) = CALLBACKS.get().and_then(|callbacks| callbacks.after_resize) {
        unsafe { callback(this, result) };
    }
    result
}

pub(crate) unsafe fn release(this: *mut c_void, _next: hooks::ReleaseContext<'_>) -> u32 {
    let swapchain_state = {
        let states = swapchains()
            .read()
            .unwrap_or_else(|error| error.into_inner());
        states.get(&(this as usize)).cloned()
    };
    if let Some(state) = swapchain_state {
        let references = unsafe { (state.release)(this) };
        if references == 0 {
            swapchains()
                .write()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&(this as usize));
            if let Some(callback) = CALLBACKS.get().and_then(|callbacks| callbacks.destroyed) {
                unsafe { callback(this) };
            }
        }
        return references;
    }
    0
}

pub(crate) fn register_runtime_hooks() {
    hooks::register_query_interface(query_interface);
    hooks::register_factory_create_swapchain(factory_created_swapchain);
    hooks::register_factory_create_swapchain_for_hwnd(factory_created_swapchain);
    hooks::register_factory_create_swapchain_for_core_window(factory_created_swapchain);
    hooks::register_factory_create_swapchain_for_composition(factory_created_swapchain);
    hooks::register_present(present);
    hooks::register_present1(present1);
    hooks::register_resize_buffers(resize_buffers);
}
