use crate::{
    hooks,
    vtable::{FactoryHook, IDXGIFactoryHook, IDXGISwapchainHook, SwapchainHook, Vtable},
};
use snowglass_hook::HookChain;
use std::{
    any::TypeId,
    collections::HashMap,
    ffi::c_void,
    sync::{Arc, LazyLock, OnceLock, RwLock},
};
use windows::{
    Win32::Graphics::Dxgi::{
        DXGI_ERROR_INVALID_CALL, IDXGIFactory, IDXGIFactory_Vtbl, IDXGIFactory1,
        IDXGIFactory1_Vtbl, IDXGIFactory2, IDXGIFactory2_Vtbl, IDXGIFactory3, IDXGIFactory3_Vtbl,
        IDXGIFactory4, IDXGIFactory4_Vtbl, IDXGIFactory5, IDXGIFactory5_Vtbl, IDXGIFactory6,
        IDXGIFactory6_Vtbl, IDXGIFactory7, IDXGIFactory7_Vtbl, IDXGISwapChain, IDXGISwapChain_Vtbl,
        IDXGISwapChain1, IDXGISwapChain1_Vtbl, IDXGISwapChain2, IDXGISwapChain2_Vtbl,
        IDXGISwapChain3, IDXGISwapChain3_Vtbl, IDXGISwapChain4, IDXGISwapChain4_Vtbl,
    },
    core::{GUID, Interface},
};

/// A monomorphized per-object install routine, used to dispatch a runtime
/// interface identity (an IID or a widest-cast result) to a typed layout.
type InstallFn = unsafe fn(*mut c_void) -> bool;

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
static SWAPCHAINS: OnceLock<RwLock<HashMap<usize, Arc<dyn SwapchainHook>>>> = OnceLock::new();
static FACTORIES: OnceLock<RwLock<HashMap<usize, Arc<dyn FactoryHook>>>> = OnceLock::new();

fn swapchains() -> &'static RwLock<HashMap<usize, Arc<dyn SwapchainHook>>> {
    SWAPCHAINS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn factories() -> &'static RwLock<HashMap<usize, Arc<dyn FactoryHook>>> {
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

/// # Safety
///
/// `swapchain` must be null or point to a live `IDXGISwapChain` interface.
pub unsafe fn install_swapchain(swapchain: *mut c_void) -> bool {
    unsafe { install_swapchain_kind::<IDXGISwapChain_Vtbl>(swapchain) }
}

/// # Safety
///
/// `swapchain` must be null or point to a live `IDXGISwapChain1` interface.
pub unsafe fn install_swapchain1(swapchain: *mut c_void) -> bool {
    unsafe { install_swapchain_kind::<IDXGISwapChain1_Vtbl>(swapchain) }
}

/// The widest swapchain interface implemented by the object, paired with the
/// install routine for that layout.
fn widest_swapchain(swapchain: &IDXGISwapChain) -> Option<(*mut c_void, InstallFn)> {
    macro_rules! try_cast {
        ($interface:ty, $vtbl:ty) => {
            if let Ok(interface) = swapchain.cast::<$interface>() {
                return Some((
                    interface.as_raw(),
                    install_swapchain_kind::<$vtbl> as InstallFn,
                ));
            }
        };
    }
    try_cast!(IDXGISwapChain4, IDXGISwapChain4_Vtbl);
    try_cast!(IDXGISwapChain3, IDXGISwapChain3_Vtbl);
    try_cast!(IDXGISwapChain2, IDXGISwapChain2_Vtbl);
    try_cast!(IDXGISwapChain1, IDXGISwapChain1_Vtbl);
    None
}

/// Installs interception and assigns the swapchain to the first backend that
/// claims its DXGI creation object.
///
/// # Safety
///
/// `swapchain` must be null or point to a live `IDXGISwapChain` interface;
/// `creation_device` must be null or point to a live COM object for the
/// duration of this call.
pub unsafe fn register_swapchain(swapchain: *mut c_void, creation_device: *mut c_void) -> bool {
    if swapchain.is_null() {
        return false;
    }
    let extended =
        unsafe { IDXGISwapChain::from_raw_borrowed(&swapchain) }.and_then(widest_swapchain);
    let mut installed = unsafe { install_swapchain(swapchain) };
    if let Some((extended, install)) = extended {
        installed |= unsafe { install(extended) };
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

unsafe fn install_swapchain_kind<A: Vtable>(swapchain: *mut c_void) -> bool {
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
            if existing.vtable_type() == TypeId::of::<A>() || existing.slots() >= A::SLOTS {
                return true;
            }
            unsafe { IDXGISwapchainHook::<A>::clone_from(existing.original_vtable()) }
        } else {
            unsafe { IDXGISwapchainHook::<A>::clone_from(current) }
        }
    } else {
        let original = unsafe { *swapchain.cast::<*const usize>() };
        unsafe { IDXGISwapchainHook::<A>::clone_from(original) }
    };
    let Some(vtable) = vtable else {
        return false;
    };
    let state: Arc<dyn SwapchainHook> = Arc::new(vtable);
    unsafe { *swapchain.cast::<*const usize>() = state.patched_vtable() };
    states.insert(key, state);
    true
}

/// # Safety
///
/// `factory` must be null or point to a live `IDXGIFactory` interface.
pub unsafe fn install_factory(factory: *mut c_void) -> bool {
    unsafe { install_factory_kind::<IDXGIFactory_Vtbl>(factory) }
}

/// Installs per-object hooks for every factory interface supported by a DXGI
/// factory.
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
        ($interface:ty, $vtbl:ty) => {
            if let Ok(interface) = factory.cast::<$interface>() {
                installed |= unsafe { install_factory_kind::<$vtbl>(interface.as_raw()) };
            }
        };
    }
    install_interface!(IDXGIFactory7, IDXGIFactory7_Vtbl);
    install_interface!(IDXGIFactory6, IDXGIFactory6_Vtbl);
    install_interface!(IDXGIFactory5, IDXGIFactory5_Vtbl);
    install_interface!(IDXGIFactory4, IDXGIFactory4_Vtbl);
    install_interface!(IDXGIFactory3, IDXGIFactory3_Vtbl);
    install_interface!(IDXGIFactory2, IDXGIFactory2_Vtbl);
    install_interface!(IDXGIFactory1, IDXGIFactory1_Vtbl);
    installed | unsafe { install_factory(factory.as_raw()) }
}

/// # Safety
///
/// `factory` must be null or point to a live COM interface identified by
/// `interface_id`, and `interface_id` must be null or point to a valid GUID.
pub unsafe fn install_factory_interface(factory: *mut c_void, interface_id: *const c_void) -> bool {
    if interface_id.is_null() {
        return false;
    }
    let interface_id = unsafe { &*interface_id.cast::<GUID>() };
    let Some(install) = factory_install(interface_id) else {
        return false;
    };
    unsafe { install(factory) }
}

unsafe fn install_factory_kind<A: Vtable>(factory: *mut c_void) -> bool {
    if factory.is_null() {
        return false;
    }
    let key = factory as usize;
    let mut states = factories()
        .write()
        .unwrap_or_else(|error| error.into_inner());
    let vtable = if let Some(existing) = states.get(&key) {
        let current = unsafe { *factory.cast::<*const usize>() };
        if current == existing.patched_vtable() {
            if existing.vtable_type() == TypeId::of::<A>() || existing.slots() >= A::SLOTS {
                return true;
            }
            unsafe { IDXGIFactoryHook::<A>::clone_from(existing.original_vtable()) }
        } else {
            unsafe { IDXGIFactoryHook::<A>::clone_and_patch(factory) }
        }
    } else {
        unsafe { IDXGIFactoryHook::<A>::clone_and_patch(factory) }
    };
    let Some(vtable) = vtable else {
        return false;
    };
    let state: Arc<dyn FactoryHook> = Arc::new(vtable);
    unsafe { *factory.cast::<*const usize>() = state.patched_vtable() };
    states.insert(key, state);
    true
}

fn factory_install(interface_id: &GUID) -> Option<InstallFn> {
    [
        (
            &IDXGIFactory::IID,
            install_factory_kind::<IDXGIFactory_Vtbl> as InstallFn,
        ),
        (
            &IDXGIFactory1::IID,
            install_factory_kind::<IDXGIFactory1_Vtbl> as InstallFn,
        ),
        (
            &IDXGIFactory2::IID,
            install_factory_kind::<IDXGIFactory2_Vtbl> as InstallFn,
        ),
        (
            &IDXGIFactory3::IID,
            install_factory_kind::<IDXGIFactory3_Vtbl> as InstallFn,
        ),
        (
            &IDXGIFactory4::IID,
            install_factory_kind::<IDXGIFactory4_Vtbl> as InstallFn,
        ),
        (
            &IDXGIFactory5::IID,
            install_factory_kind::<IDXGIFactory5_Vtbl> as InstallFn,
        ),
        (
            &IDXGIFactory6::IID,
            install_factory_kind::<IDXGIFactory6_Vtbl> as InstallFn,
        ),
        (
            &IDXGIFactory7::IID,
            install_factory_kind::<IDXGIFactory7_Vtbl> as InstallFn,
        ),
    ]
    .into_iter()
    .find_map(|(candidate, install)| (interface_id == candidate).then_some(install))
}

fn swapchain_install(interface_id: &GUID) -> Option<InstallFn> {
    [
        (
            &IDXGISwapChain1::IID,
            install_swapchain_kind::<IDXGISwapChain1_Vtbl> as InstallFn,
        ),
        (
            &IDXGISwapChain2::IID,
            install_swapchain_kind::<IDXGISwapChain2_Vtbl> as InstallFn,
        ),
        (
            &IDXGISwapChain3::IID,
            install_swapchain_kind::<IDXGISwapChain3_Vtbl> as InstallFn,
        ),
        (
            &IDXGISwapChain4::IID,
            install_swapchain_kind::<IDXGISwapChain4_Vtbl> as InstallFn,
        ),
    ]
    .into_iter()
    .find_map(|(candidate, install)| (interface_id == candidate).then_some(install))
}

pub(crate) fn swapchain_method<T: Copy>(
    swapchain: *mut c_void,
    select: impl FnOnce(&dyn SwapchainHook) -> T,
) -> Option<T> {
    let states = swapchains()
        .read()
        .unwrap_or_else(|error| error.into_inner());
    states
        .get(&(swapchain as usize))
        .map(|state| select(&**state))
}

pub(crate) fn factory_method<T: Copy>(
    factory: *mut c_void,
    select: impl FnOnce(&dyn FactoryHook) -> Option<T>,
) -> Option<T> {
    let states = factories()
        .read()
        .unwrap_or_else(|error| error.into_inner());
    states
        .get(&(factory as usize))
        .and_then(|state| select(&**state))
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

pub(crate) unsafe fn created_factory(
    _interface_id: *const c_void,
    factory: *mut *mut c_void,
    result: i32,
) {
    if result < 0 || factory.is_null() {
        return;
    }
    let factory = unsafe { *factory };
    if factory.is_null() {
        return;
    }
    unsafe { install_factory_interfaces(factory) };
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
            } else if let Some(install) = swapchain_install(interface_id) {
                unsafe { install(swapchain) };
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
    hooks::register_create_factory(created_factory);
    hooks::register_create_factory1(created_factory);
    hooks::register_create_factory2(created_factory);
    hooks::register_query_interface(query_interface);
    hooks::register_factory_create_swapchain(factory_created_swapchain);
    hooks::register_factory_create_swapchain_for_hwnd(factory_created_swapchain);
    hooks::register_factory_create_swapchain_for_core_window(factory_created_swapchain);
    hooks::register_factory_create_swapchain_for_composition(factory_created_swapchain);
    hooks::register_present(present);
    hooks::register_present1(present1);
    hooks::register_resize_buffers(resize_buffers);
}
