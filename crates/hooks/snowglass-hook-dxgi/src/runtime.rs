use crate::{
    hooks,
    vtable::{FactoryHook, IDXGIFactoryHook, IDXGISwapchainHook, SwapchainHook, Vtable},
};
use snowglass_hook::HookChain;
use std::{
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

/// Per-swapchain capture state owned by a backend.
///
/// A backend's [`SwapchainClaim`] produces one of these when it accepts a
/// swapchain; it is erased into a [`SwapchainOwnerHandle`], stored in the
/// swapchain's hook context, and driven from the present, resize, and release
/// trampolines. The object carries all per-swapchain state as `self`, so no
/// opaque context pointer is threaded alongside the callbacks.
pub trait SwapchainOwner: Send + Sync + 'static {
    /// Runs just before the swapchain presents.
    ///
    /// # Safety
    ///
    /// `swapchain` points to the live swapchain being presented.
    unsafe fn before_present(&self, swapchain: *mut c_void);

    /// Runs just before the swapchain resizes its buffers.
    ///
    /// # Safety
    ///
    /// `swapchain` points to the live swapchain being resized.
    unsafe fn before_resize(&self, _swapchain: *mut c_void) {}

    /// Runs just after `ResizeBuffers` returns, with its result code.
    ///
    /// # Safety
    ///
    /// `swapchain` points to the live swapchain that was resized.
    unsafe fn after_resize(&self, _swapchain: *mut c_void, _result: i32) {}

    /// Runs when the swapchain is finally released, before the owner is dropped.
    ///
    /// # Safety
    ///
    /// `swapchain` points to the swapchain being destroyed.
    unsafe fn destroyed(&self, _swapchain: *mut c_void) {}
}

/// The C-ABI dispatch table for a [`SwapchainOwnerHandle`]. Every slot is an
/// `extern "system"` shim, so a handle built in one hook DLL can be called by
/// the central DXGI hook DLL without assuming a shared Rust ABI between them.
#[repr(C)]
pub struct SwapchainOwnerVtable {
    before_present: unsafe extern "system" fn(*mut c_void, *mut c_void),
    before_resize: unsafe extern "system" fn(*mut c_void, *mut c_void),
    after_resize: unsafe extern "system" fn(*mut c_void, *mut c_void, i32),
    destroyed: unsafe extern "system" fn(*mut c_void, *mut c_void),
    drop: unsafe extern "system" fn(*mut c_void),
}

/// A type-erased, C-ABI per-swapchain owner exchanged across the hook-DLL
/// boundary.
///
/// This is a hand-rolled trait object: `context` is a `Box`-owned backend
/// value and `vtable` holds `extern "system"` shims that dispatch to it,
/// including a destructor. Both the shims and the backend allocation live in
/// the backend's DLL; the DXGI hook only ever calls through the vtable, so the
/// contract is pure C ABI and does not depend on the two DLLs sharing a Rust
/// toolchain.
#[repr(C)]
pub struct SwapchainOwnerHandle {
    context: *mut c_void,
    vtable: &'static SwapchainOwnerVtable,
}

// SAFETY: `new` only accepts `T: SwapchainOwner`, which is `Send + Sync`, and
// the vtable is an immutable `'static`. The handle owns its context like a
// `Box<T>`, so moving it between threads and sharing `&` is sound.
unsafe impl Send for SwapchainOwnerHandle {}
unsafe impl Sync for SwapchainOwnerHandle {}

impl SwapchainOwnerHandle {
    /// Erases a concrete backend owner into a C-ABI handle. The dispatch shims
    /// are monomorphized into the caller's crate, so they run the backend's own
    /// code and allocator.
    pub fn new<T: SwapchainOwner>(owner: T) -> Self {
        unsafe extern "system" fn before_present<T: SwapchainOwner>(
            context: *mut c_void,
            swapchain: *mut c_void,
        ) {
            unsafe { (*context.cast::<T>()).before_present(swapchain) };
        }
        unsafe extern "system" fn before_resize<T: SwapchainOwner>(
            context: *mut c_void,
            swapchain: *mut c_void,
        ) {
            unsafe { (*context.cast::<T>()).before_resize(swapchain) };
        }
        unsafe extern "system" fn after_resize<T: SwapchainOwner>(
            context: *mut c_void,
            swapchain: *mut c_void,
            result: i32,
        ) {
            unsafe { (*context.cast::<T>()).after_resize(swapchain, result) };
        }
        unsafe extern "system" fn destroyed<T: SwapchainOwner>(
            context: *mut c_void,
            swapchain: *mut c_void,
        ) {
            unsafe { (*context.cast::<T>()).destroyed(swapchain) };
        }
        unsafe extern "system" fn drop_owner<T: SwapchainOwner>(context: *mut c_void) {
            drop(unsafe { Box::from_raw(context.cast::<T>()) });
        }
        SwapchainOwnerHandle {
            context: Box::into_raw(Box::new(owner)).cast::<c_void>(),
            vtable: &const {
                SwapchainOwnerVtable {
                    before_present: before_present::<T>,
                    before_resize: before_resize::<T>,
                    after_resize: after_resize::<T>,
                    destroyed: destroyed::<T>,
                    drop: drop_owner::<T>,
                }
            },
        }
    }

    /// # Safety
    ///
    /// `swapchain` points to the live swapchain being presented.
    unsafe fn before_present(&self, swapchain: *mut c_void) {
        unsafe { (self.vtable.before_present)(self.context, swapchain) };
    }

    /// # Safety
    ///
    /// `swapchain` points to the live swapchain being resized.
    unsafe fn before_resize(&self, swapchain: *mut c_void) {
        unsafe { (self.vtable.before_resize)(self.context, swapchain) };
    }

    /// # Safety
    ///
    /// `swapchain` points to the live swapchain that was resized.
    unsafe fn after_resize(&self, swapchain: *mut c_void, result: i32) {
        unsafe { (self.vtable.after_resize)(self.context, swapchain, result) };
    }

    /// # Safety
    ///
    /// `swapchain` points to the swapchain being destroyed.
    unsafe fn destroyed(&self, swapchain: *mut c_void) {
        unsafe { (self.vtable.destroyed)(self.context, swapchain) };
    }
}

impl Drop for SwapchainOwnerHandle {
    fn drop(&mut self) {
        unsafe { (self.vtable.drop)(self.context) };
    }
}

/// A backend entry point tried against each newly created swapchain. On a claim
/// it writes the per-swapchain owner into `out` and returns `true`; otherwise it
/// leaves `out` untouched and returns `false`.
///
/// # Safety
///
/// The two COM pointers must be null or point to live interfaces for the
/// duration of the call, and `out` must point to writable, uninitialized
/// storage for a [`SwapchainOwnerHandle`].
pub type SwapchainClaim =
    unsafe extern "system" fn(*mut c_void, *mut c_void, *mut SwapchainOwnerHandle) -> bool;

static CLAIMS: LazyLock<RwLock<Vec<SwapchainClaim>>> = LazyLock::new(Default::default);
static SWAPCHAINS: OnceLock<RwLock<HashMap<usize, Arc<dyn SwapchainHook>>>> = OnceLock::new();
static FACTORIES: OnceLock<RwLock<HashMap<usize, Arc<dyn FactoryHook>>>> = OnceLock::new();

fn swapchains() -> &'static RwLock<HashMap<usize, Arc<dyn SwapchainHook>>> {
    SWAPCHAINS.get_or_init(|| RwLock::new(HashMap::new()))
}

fn factories() -> &'static RwLock<HashMap<usize, Arc<dyn FactoryHook>>> {
    FACTORIES.get_or_init(|| RwLock::new(HashMap::new()))
}

pub fn register_claim(claim: SwapchainClaim) {
    let mut registered = CLAIMS.write().unwrap_or_else(|error| error.into_inner());
    if !registered
        .iter()
        .any(|existing| *existing as usize == claim as usize)
    {
        registered.push(claim);
    }
}

/// The widest swapchain interface implemented by the object, paired with the
/// install routine for that layout. The base `IDXGISwapChain` is installed
/// separately, so it is omitted here.
fn widest_swapchain(swapchain: &IDXGISwapChain) -> Option<(*mut c_void, InstallFn)> {
    snowglass_hook::widest_interfaces! {
        from swapchain => InstallFn;
        install install_swapchain;
        IDXGISwapChain4 => IDXGISwapChain4_Vtbl,
        IDXGISwapChain3 => IDXGISwapChain3_Vtbl,
        IDXGISwapChain2 => IDXGISwapChain2_Vtbl,
        IDXGISwapChain1 => IDXGISwapChain1_Vtbl,
    }
    .into_iter()
    .next()
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
    let mut installed = unsafe { install_swapchain::<IDXGISwapChain_Vtbl>(swapchain) };
    if let Some((extended, install)) = extended {
        installed |= unsafe { install(extended) };
    }
    let owner = CLAIMS
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .iter()
        .find_map(|claim| {
            let mut handle = std::mem::MaybeUninit::<SwapchainOwnerHandle>::uninit();
            unsafe { claim(swapchain, creation_device, handle.as_mut_ptr()) }
                .then(|| unsafe { handle.assume_init() })
        });
    if let Some(owner) = owner {
        // The base and widest interface pointers share one owner, so a present
        // through either resolves the same capture state.
        let owner = Arc::new(owner);
        if let Some((extended, _)) = extended {
            set_swapchain_owner(extended, Some(owner.clone()));
        }
        set_swapchain_owner(swapchain, Some(owner));
    }
    installed
}

/// The backend that claimed a hooked swapchain, if any. The owner is cloned out
/// so callbacks run without holding the swapchain or hook-context locks.
fn swapchain_owner(swapchain: *mut c_void) -> Option<Arc<SwapchainOwnerHandle>> {
    swapchain_method(swapchain, |hook| {
        hook.owner()
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    })
    .flatten()
}

fn set_swapchain_owner(swapchain: *mut c_void, owner: Option<Arc<SwapchainOwnerHandle>>) {
    let states = swapchains()
        .read()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(state) = states.get(&(swapchain as usize)) {
        *state
            .owner()
            .write()
            .unwrap_or_else(|error| error.into_inner()) = owner;
    }
}

/// Installs interception on a swapchain by cloning the vtable layout of
/// interface generation `A`.
///
/// # Safety
///
/// `swapchain` must be null or point to a live object implementing the
/// swapchain interface generation named by `A`.
pub(crate) unsafe fn install_swapchain<A: Vtable>(swapchain: *mut c_void) -> bool {
    if swapchain.is_null() {
        return false;
    }
    let key = swapchain as usize;
    let mut states = swapchains()
        .write()
        .unwrap_or_else(|error| error.into_inner());
    let vtable = if let Some(existing) = states.get(&key) {
        // A widening re-clone replaces the hook object; carry the recorded
        // owner into the replacement.
        let owner = RwLock::new(
            existing
                .owner()
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .clone(),
        );
        let current = unsafe { *swapchain.cast::<*const usize>() };
        if current == existing.patched_vtable() {
            if A::supported_by(existing.vtable_type()) {
                return true;
            }
            unsafe { IDXGISwapchainHook::<A>::clone_from(existing.original_vtable(), owner) }
        } else {
            unsafe { IDXGISwapchainHook::<A>::clone_from(current, owner) }
        }
    } else {
        let original = unsafe { *swapchain.cast::<*const usize>() };
        unsafe { IDXGISwapchainHook::<A>::clone_from(original, RwLock::new(None)) }
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
    let interfaces = snowglass_hook::widest_interfaces! {
        from factory => InstallFn;
        install install_factory_kind;
        IDXGIFactory7 => IDXGIFactory7_Vtbl,
        IDXGIFactory6 => IDXGIFactory6_Vtbl,
        IDXGIFactory5 => IDXGIFactory5_Vtbl,
        IDXGIFactory4 => IDXGIFactory4_Vtbl,
        IDXGIFactory3 => IDXGIFactory3_Vtbl,
        IDXGIFactory2 => IDXGIFactory2_Vtbl,
        IDXGIFactory1 => IDXGIFactory1_Vtbl,
        IDXGIFactory => IDXGIFactory_Vtbl,
    };
    interfaces
        .into_iter()
        .fold(false, |installed, (pointer, install)| {
            installed | unsafe { install(pointer) }
        })
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
            if A::supported_by(existing.vtable_type()) {
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
            &IDXGISwapChain::IID,
            install_swapchain::<IDXGISwapChain_Vtbl> as InstallFn,
        ),
        (
            &IDXGISwapChain1::IID,
            install_swapchain::<IDXGISwapChain1_Vtbl> as InstallFn,
        ),
        (
            &IDXGISwapChain2::IID,
            install_swapchain::<IDXGISwapChain2_Vtbl> as InstallFn,
        ),
        (
            &IDXGISwapChain3::IID,
            install_swapchain::<IDXGISwapChain3_Vtbl> as InstallFn,
        ),
        (
            &IDXGISwapChain4::IID,
            install_swapchain::<IDXGISwapChain4_Vtbl> as InstallFn,
        ),
    ]
    .into_iter()
    .find_map(|(candidate, install)| (interface_id == candidate).then_some(install))
}

pub(crate) fn swapchain_method<T>(
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
            let owner = swapchain_owner(this);
            if let Some(install) = swapchain_install(interface_id) {
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
                set_swapchain_owner(swapchain, Some(owner));
            }
        }
    }
    result
}

/// Runs a producer callback, absorbing panics. A panic must never unwind
/// through the detour into the host game (that aborts the process), so the
/// swapchain's owner is cleared instead: capture stops, the game lives.
fn guarded(this: *mut c_void, stage: &str, run: impl FnOnce()) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run));
    if result.is_err() {
        set_swapchain_owner(this, None);
        crate::dxgi_log(format!(
            "snowglass: {stage} callback panicked; capture disabled for swapchain {this:p}"
        ));
    }
}

pub(crate) unsafe fn present(
    this: *mut c_void,
    sync_interval: u32,
    flags: u32,
    mut next: hooks::PresentContext<'_>,
) -> i32 {
    if let Some(owner) = swapchain_owner(this) {
        guarded(this, "before_present", || unsafe {
            owner.before_present(this)
        });
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
    if let Some(owner) = swapchain_owner(this) {
        guarded(this, "before_present", || unsafe {
            owner.before_present(this)
        });
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
    let owner = swapchain_owner(this);
    if let Some(owner) = &owner {
        guarded(this, "before_resize", || unsafe {
            owner.before_resize(this)
        });
    }
    let Some(continuation) = next.fp_next().copied() else {
        return DXGI_ERROR_INVALID_CALL.0;
    };
    let result = unsafe { continuation(this, buffer_count, width, height, format, flags, next) };
    if let Some(owner) = &owner {
        guarded(this, "after_resize", || unsafe {
            owner.after_resize(this, result)
        });
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
            let owner = state
                .owner()
                .write()
                .unwrap_or_else(|error| error.into_inner())
                .take();
            if let Some(owner) = owner {
                // `owner` is the last reference; dropping it after `destroyed`
                // releases the backend's per-swapchain capture state.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
                    owner.destroyed(this)
                }));
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
