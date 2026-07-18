use crate::{
    hooks,
    vtable::{FactoryKind, IDXGIFactoryHook, SwapchainKind, IDXGISwapchainHook},
};
use snowglass_hook::HookChain;
use std::{
    collections::HashMap,
    ffi::c_void,
    sync::{Arc, LazyLock, OnceLock, RwLock},
};
use windows::{
    Win32::Graphics::Dxgi::{
        DXGI_ERROR_INVALID_CALL, IDXGIFactory, IDXGIFactory1, IDXGIFactory2, IDXGIFactory3,
        IDXGIFactory4, IDXGIFactory5, IDXGIFactory6, IDXGIFactory7, IDXGISwapChain,
        IDXGISwapChain1, IDXGISwapChain2, IDXGISwapChain3, IDXGISwapChain4,
    },
    core::{GUID, Interface},
};

#[derive(Clone, Copy)]
pub struct SwapchainCallbacks {
    pub claim: unsafe fn(*mut c_void, *mut c_void) -> bool,
    pub before_present: Option<unsafe fn(*mut c_void)>,
    pub before_resize: Option<unsafe fn(*mut c_void)>,
    pub after_resize: Option<unsafe fn(*mut c_void, i32)>,
    pub destroyed: Option<unsafe fn(*mut c_void)>,
}

static CALLBACKS: LazyLock<RwLock<Vec<SwapchainCallbacks>>> = LazyLock::new(Default::default);
static OWNERS: LazyLock<RwLock<HashMap<usize, SwapchainCallbacks>>> =
    LazyLock::new(Default::default);
static SWAPCHAINS: OnceLock<RwLock<HashMap<usize, Arc<IDXGISwapchainHook>>>> = OnceLock::new();
static FACTORIES: OnceLock<RwLock<HashMap<usize, Arc<IDXGIFactoryHook>>>> = OnceLock::new();

fn swapchains() -> &'static RwLock<HashMap<usize, Arc<IDXGISwapchainHook>>> {
    SWAPCHAINS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn factories() -> &'static RwLock<HashMap<usize, Arc<IDXGIFactoryHook>>> {
    FACTORIES.get_or_init(|| RwLock::new(HashMap::new()))
}

pub fn register_callbacks(callbacks: SwapchainCallbacks) {
    let mut registered = CALLBACKS.write().unwrap_or_else(|error| error.into_inner());
    if !registered
        .iter()
        .any(|existing| existing.claim as usize == callbacks.claim as usize)
    {
        registered.push(callbacks);
    }
}

pub unsafe fn install_swapchain(swapchain: *mut c_void) -> bool {
    unsafe { install_swapchain_kind(swapchain, SwapchainKind::Swapchain) }
}

pub unsafe fn install_swapchain1(swapchain: *mut c_void) -> bool {
    unsafe { install_swapchain_kind(swapchain, SwapchainKind::Swapchain1) }
}

/// Installs interception and assigns the swapchain to the first backend that
/// claims its DXGI creation object.
pub unsafe fn register_swapchain(swapchain: *mut c_void, creation_device: *mut c_void) -> bool {
    if swapchain.is_null() {
        return false;
    }
    let extended = unsafe { IDXGISwapChain::from_raw_borrowed(&swapchain) }.and_then(|swapchain| {
        if let Ok(interface) = swapchain.cast::<IDXGISwapChain4>() {
            Some((interface.as_raw(), SwapchainKind::Swapchain4))
        } else if let Ok(interface) = swapchain.cast::<IDXGISwapChain3>() {
            Some((interface.as_raw(), SwapchainKind::Swapchain3))
        } else if let Ok(interface) = swapchain.cast::<IDXGISwapChain2>() {
            Some((interface.as_raw(), SwapchainKind::Swapchain2))
        } else {
            swapchain
                .cast::<IDXGISwapChain1>()
                .ok()
                .map(|interface| (interface.as_raw(), SwapchainKind::Swapchain1))
        }
    });
    let mut installed = unsafe { install_swapchain(swapchain) };
    if let Some((extended, kind)) = extended {
        installed |= unsafe { install_swapchain_kind(extended, kind) };
    }
    let owner = CALLBACKS
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .iter()
        .copied()
        .find(|callbacks| unsafe { (callbacks.claim)(swapchain, creation_device) });
    if let Some(owner) = owner {
        let mut owners = OWNERS.write().unwrap_or_else(|error| error.into_inner());
        owners.insert(swapchain as usize, owner);
        if let Some((extended, _)) = extended {
            owners.insert(extended as usize, owner);
        }
    }
    installed
}

unsafe fn install_swapchain_kind(swapchain: *mut c_void, kind: SwapchainKind) -> bool {
    if swapchain.is_null() {
        return false;
    }
    let key = swapchain as usize;
    let mut states = swapchains()
        .write()
        .unwrap_or_else(|error| error.into_inner());
    let vtable = if let Some(existing) = states.get(&key) {
        let current = unsafe { *swapchain.cast::<*const usize>() };
        if current == existing.patched_vtable() {
            if existing.kind() >= kind {
                return true;
            }
            unsafe { IDXGISwapchainHook::clone_from(existing.original_vtable(), kind) }
        } else {
            unsafe { IDXGISwapchainHook::clone_from(current, kind) }
        }
    } else {
        let original = unsafe { *swapchain.cast::<*const usize>() };
        unsafe { IDXGISwapchainHook::clone_from(original, kind) }
    };
    let Some(vtable) = vtable else {
        return false;
    };
    let state = Arc::new(vtable);
    unsafe { *swapchain.cast::<*const usize>() = state.patched_vtable() };
    states.insert(key, state);
    true
}

pub unsafe fn install_factory(factory: *mut c_void) -> bool {
    unsafe { install_factory_kind(factory, FactoryKind::Factory) }
}

/// Seeds process-wide Factory2-compatible method detours without modifying the
/// supplied factory object. This is suitable for a short-lived helper factory.
pub unsafe fn install_factory_detours(factory: *mut c_void) -> bool {
    let Some(factory) = (unsafe { IDXGIFactory::from_raw_borrowed(&factory) }) else {
        return false;
    };
    let Ok(factory2) = factory.cast::<IDXGIFactory2>() else {
        return false;
    };
    unsafe { crate::detour::install_create_swapchain_for_hwnd(factory2.as_raw()) }
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
    let Some(kind) = factory_kind(interface_id) else {
        return false;
    };
    unsafe { install_factory_kind(factory, kind) }
}

unsafe fn install_factory_kind(factory: *mut c_void, kind: FactoryKind) -> bool {
    if factory.is_null() {
        return false;
    }
    if kind >= FactoryKind::Factory2 {
        unsafe { crate::detour::install_create_swapchain_for_hwnd(factory) };
    }
    let key = factory as usize;
    let mut states = factories()
        .write()
        .unwrap_or_else(|error| error.into_inner());
    let vtable = if let Some(existing) = states.get(&key) {
        let current = unsafe { *factory.cast::<*const usize>() };
        if current == existing.patched_vtable() {
            if existing.kind() >= kind {
                return true;
            }
            unsafe { IDXGIFactoryHook::clone_from(existing.original_vtable(), kind) }
        } else {
            unsafe { IDXGIFactoryHook::clone_and_patch(factory, kind) }
        }
    } else {
        unsafe { IDXGIFactoryHook::clone_and_patch(factory, kind) }
    };
    let Some(vtable) = vtable else {
        return false;
    };
    let state = Arc::new(vtable);
    unsafe { *factory.cast::<*const usize>() = state.patched_vtable() };
    states.insert(key, state);
    true
}

fn factory_kind(interface_id: &GUID) -> Option<FactoryKind> {
    [
        (&IDXGIFactory::IID, FactoryKind::Factory),
        (&IDXGIFactory1::IID, FactoryKind::Factory1),
        (&IDXGIFactory2::IID, FactoryKind::Factory2),
        (&IDXGIFactory3::IID, FactoryKind::Factory3),
        (&IDXGIFactory4::IID, FactoryKind::Factory4),
        (&IDXGIFactory5::IID, FactoryKind::Factory5),
        (&IDXGIFactory6::IID, FactoryKind::Factory6),
        (&IDXGIFactory7::IID, FactoryKind::Factory7),
    ]
    .into_iter()
    .find_map(|(candidate, kind)| (interface_id == candidate).then_some(kind))
}

pub(crate) fn swapchain_method<T: Copy>(
    swapchain: *mut c_void,
    select: impl FnOnce(&IDXGISwapchainHook) -> T,
) -> Option<T> {
    let states = swapchains()
        .read()
        .unwrap_or_else(|error| error.into_inner());
    states.get(&(swapchain as usize)).map(|state| select(state))
}

pub(crate) fn factory_method<T: Copy>(
    factory: *mut c_void,
    select: impl FnOnce(&IDXGIFactoryHook) -> Option<T>,
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
    if let Some(method) = swapchain_method(factory_or_swapchain, |vtable| vtable.query_interface())
    {
        return Some(method);
    }
    factory_method(factory_or_swapchain, |vtable| {
        Some(vtable.query_interface())
    })
}

pub(crate) unsafe fn factory_created_swapchain(
    device: *mut c_void,
    swapchain: *mut *mut c_void,
    result: i32,
) {
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
    unsafe { register_swapchain(swapchain, device) };
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
            let owner = OWNERS
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .get(&(this as usize))
                .copied();
            if interface_id == &IDXGISwapChain::IID {
                unsafe { install_swapchain(swapchain) };
            } else if let Some(kind) = [
                (&IDXGISwapChain1::IID, SwapchainKind::Swapchain1),
                (&IDXGISwapChain2::IID, SwapchainKind::Swapchain2),
                (&IDXGISwapChain3::IID, SwapchainKind::Swapchain3),
                (&IDXGISwapChain4::IID, SwapchainKind::Swapchain4),
            ]
            .into_iter()
            .find_map(|(candidate, kind)| (interface_id == candidate).then_some(kind))
            {
                unsafe { install_swapchain_kind(swapchain, kind) };
            } else {
                unsafe {
                    install_factory_interface(
                        swapchain,
                        (interface_id as *const GUID).cast::<c_void>(),
                    )
                };
            }
            if let Some(owner) = owner {
                OWNERS
                    .write()
                    .unwrap_or_else(|error| error.into_inner())
                    .insert(swapchain as usize, owner);
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
    let callback = {
        let owners = OWNERS.read().unwrap_or_else(|error| error.into_inner());
        owners
            .get(&(this as usize))
            .and_then(|callbacks| callbacks.before_present)
    };
    if let Some(callback) = callback {
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
    let callback = {
        let owners = OWNERS.read().unwrap_or_else(|error| error.into_inner());
        owners
            .get(&(this as usize))
            .and_then(|callbacks| callbacks.before_present)
    };
    if let Some(callback) = callback {
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
    let owner = OWNERS
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .get(&(this as usize))
        .copied();
    if let Some(callback) = owner.and_then(|callbacks| callbacks.before_resize) {
        unsafe { callback(this) };
    }
    let Some(continuation) = next.fp_next().copied() else {
        return DXGI_ERROR_INVALID_CALL.0;
    };
    let result = unsafe { continuation(this, buffer_count, width, height, format, flags, next) };
    if let Some(callback) = owner.and_then(|callbacks| callbacks.after_resize) {
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
        // The cloned vtable spans only the COM interface methods, but a final
        // release destroys the object through implementation virtuals stored
        // beyond them (dxgi.dll dispatches its deleting destructor from the
        // object's vtable). Restore the original vtable across the downstream
        // release so destruction never reads past the clone.
        unsafe { *this.cast::<*const usize>() = state.original_vtable() };
        let references = unsafe { (state.release())(this) };
        if references == 0 {
            swapchains()
                .write()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&(this as usize));
            let owner = OWNERS
                .write()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&(this as usize));
            if let Some(callback) = owner.and_then(|callbacks| callbacks.destroyed) {
                unsafe { callback(this) };
            }
        } else {
            let states = swapchains()
                .read()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(current) = states.get(&(this as usize)) {
                unsafe { *this.cast::<*const usize>() = current.patched_vtable() };
            }
        }
        return references;
    }
    0
}

pub(crate) unsafe fn factory_release(
    this: *mut c_void,
    _next: hooks::FactoryReleaseContext<'_>,
) -> u32 {
    let factory_state = {
        let states = factories()
            .read()
            .unwrap_or_else(|error| error.into_inner());
        states.get(&(this as usize)).cloned()
    };
    if let Some(state) = factory_state {
        // See `release`: factory destruction must also run on the original,
        // full-length vtable rather than the interface-only clone.
        unsafe { *this.cast::<*const usize>() = state.original_vtable() };
        let references = unsafe { (state.release())(this) };
        if references == 0 {
            factories()
                .write()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&(this as usize));
        } else {
            let states = factories()
                .read()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(current) = states.get(&(this as usize)) {
                unsafe { *this.cast::<*const usize>() = current.patched_vtable() };
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
