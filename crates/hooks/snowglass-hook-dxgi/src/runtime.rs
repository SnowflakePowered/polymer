use crate::vtable::{
    IDXGIFactoryHook, IDXGIFactoryMediaHook, IDXGISwapchainHook, SwapchainContext, SwapchainHook,
};
use parking_lot::RwLock;
use std::{
    ffi::c_void,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicU64, Ordering},
    },
};
use windows::{
    Win32::Graphics::Dxgi::{
        DXGI_PRESENT_ALLOW_TEARING, DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING, IDXGIDevice1, IDXGIFactory,
        IDXGISwapChain,
    },
    core::{BOOL, GUID, IUnknown, Interface},
};

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
pub fn register_claim(claim: SwapchainClaim) {
    let mut registered = CLAIMS.write();
    if !registered
        .iter()
        .any(|existing| *existing as usize == claim as usize)
    {
        registered.push(claim);
    }
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
    let allow_tearing = unsafe { swapchain_allows_tearing(swapchain) };
    let context = SwapchainContext::new(allow_tearing);
    let installed =
        unsafe { IDXGISwapchainHook::install_all_interfaces(swapchain, context.clone()) };
    let owner = CLAIMS.read().iter().find_map(|claim| {
        let mut handle = std::mem::MaybeUninit::<SwapchainOwnerHandle>::uninit();
        unsafe { claim(swapchain, creation_device, handle.as_mut_ptr()) }
            .then(|| unsafe { handle.assume_init() })
    });
    if let Some(owner) = owner {
        context.set_owner(Some(Arc::new(owner)));
    }
    if snowglass_hook::config::present_unthrottled() {
        unsafe { set_maximum_frame_latency(swapchain, creation_device) };
    }
    installed
}

unsafe fn swapchain_allows_tearing(swapchain: *mut c_void) -> bool {
    let Some(swapchain) = (unsafe { IDXGISwapChain::from_raw_borrowed(&swapchain) }) else {
        return false;
    };
    unsafe { swapchain.GetDesc() }
        .is_ok_and(|desc| desc.Flags & DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 as u32 != 0)
}

unsafe fn swapchain_is_windowed(swapchain: *mut c_void) -> bool {
    let Some(swapchain) = (unsafe { IDXGISwapChain::from_raw_borrowed(&swapchain) }) else {
        return false;
    };
    let mut fullscreen = BOOL(0);
    unsafe { swapchain.GetFullscreenState(Some(&mut fullscreen), None) }.is_ok()
        && !fullscreen.as_bool()
}

/// The deepest present queue DXGI accepts.
///
/// Capture runs in `before_present`, so a frame is already in the ring before
/// the guest's `Present` is forwarded at all: how many presents DXGI has queued
/// behind it cannot make the captured frame any older. All the queue depth
/// governs is how often `Present` blocks the guest's render thread.
///
/// A shallow queue therefore buys nothing and costs pacing. Holding the guest
/// at one queued frame puts its frame timing back under the display's control —
/// each `Present` waits out a full refresh interval, inherited jitter and all —
/// which is exactly what forwarding a zero sync interval set out to remove. A
/// deep queue lets `Present` return immediately and leaves the guest's own
/// frame limiter as the only thing pacing capture.
const GUEST_FRAME_LATENCY: u32 = 16;

unsafe fn set_maximum_frame_latency(swapchain: *mut c_void, creation_device: *mut c_void) {
    let from_creation = (!creation_device.is_null())
        .then(|| unsafe { IUnknown::from_raw_borrowed(&creation_device) })
        .flatten()
        .and_then(|device| device.cast::<IDXGIDevice1>().ok());
    let device = from_creation.or_else(|| {
        let swapchain = unsafe { IDXGISwapChain::from_raw_borrowed(&swapchain) }?;
        let device: windows::core::Result<IDXGIDevice1> = unsafe { swapchain.GetDevice() };
        device.ok()
    });
    let Some(device) = device else {
        tracing::debug!("snowglass: IDXGIDevice1 unavailable; frame latency left at its default");
        return;
    };
    match unsafe { device.SetMaximumFrameLatency(GUEST_FRAME_LATENCY) } {
        Ok(()) => tracing::debug!(
            "snowglass: set DXGI maximum frame latency to {GUEST_FRAME_LATENCY} so presents do not pace the guest"
        ),
        Err(error) => tracing::debug!(
            "snowglass: IDXGIDevice1::SetMaximumFrameLatency({GUEST_FRAME_LATENCY}) failed: {error}"
        ),
    }
}

fn set_swapchain_owner(swapchain: *mut c_void, owner: Option<Arc<SwapchainOwnerHandle>>) {
    IDXGISwapchainHook::with_state(swapchain, |state| state.set_owner(owner));
}

/// # Safety
///
/// `factory` must be null or point to a live `IDXGIFactory` interface.
pub unsafe fn install_factory(factory: *mut c_void) -> bool {
    unsafe { IDXGIFactoryHook::install_object::<IDXGIFactory>(factory) }
}

/// Installs per-object hooks for every factory interface supported by a DXGI
/// factory.
///
/// # Safety
///
/// `factory` must be null or point to a live `IDXGIFactory` interface for the
/// duration of this call.
pub unsafe fn install_factory_interfaces(factory: *mut c_void) -> bool {
    unsafe { IDXGIFactoryHook::install_all_interfaces(factory) }
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
    if unsafe { IDXGIFactoryHook::install_interface(factory, interface_id) } {
        return true;
    }
    // `IDXGIFactoryMedia` is only ever reached by querying a factory, so this
    // is the one place it can be caught.
    unsafe { IDXGIFactoryMediaHook::install_interface(factory, interface_id) }
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

pub(crate) unsafe fn factory_created_swapchain<C>(
    device: *mut c_void,
    swapchain: *mut *mut c_void,
    result: i32,
    _context: &C,
) {
    tracing::debug!("snowglass: DXGI swapchain creation returned {result:#x}");
    if result < 0 || swapchain.is_null() {
        return;
    }
    let swapchain = unsafe { *swapchain };
    if swapchain.is_null() {
        return;
    }
    unsafe { register_swapchain(swapchain, device) };
}

pub(crate) unsafe fn swapchain_query_interface(
    this: *mut c_void,
    interface_id: *const c_void,
    output: *mut *mut c_void,
    mut next: crate::vtable::SwapchainQueryInterfaceContext<'_>,
) -> i32 {
    let context = next.state.clone();
    let result = snowglass_hook::next!(next, this, interface_id, output);
    if result >= 0 && !interface_id.is_null() && !output.is_null() {
        let interface_id = unsafe { &*interface_id.cast::<GUID>() };
        let swapchain = unsafe { *output };
        if !swapchain.is_null() {
            if !unsafe { IDXGISwapchainHook::install_interface(swapchain, interface_id, context) } {
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

pub(crate) unsafe fn factory_query_interface(
    this: *mut c_void,
    interface_id: *const c_void,
    output: *mut *mut c_void,
    mut next: crate::vtable::FactoryQueryInterfaceContext<'_>,
) -> i32 {
    let result = snowglass_hook::next!(next, this, interface_id, output);
    if result >= 0 && !interface_id.is_null() && !output.is_null() {
        let output = unsafe { *output };
        if !output.is_null() {
            unsafe { install_factory_interface(output, interface_id) };
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
        tracing::debug!(
            "snowglass: {stage} callback panicked; capture disabled for swapchain {this:p}"
        );
    }
}

pub(crate) unsafe fn before_present(this: *mut c_void, owner: &Option<Arc<SwapchainOwnerHandle>>) {
    let owner = owner.clone();
    if let Some(owner) = owner {
        guarded(this, "before_present", || unsafe {
            owner.before_present(this)
        });
    }
}

fn forwarded_present_arguments(
    sync_interval: u32,
    flags: u32,
    unthrottled: bool,
    allow_tearing: bool,
) -> (u32, u32) {
    if !unthrottled {
        return (sync_interval, flags);
    }
    (
        0,
        if allow_tearing {
            flags | DXGI_PRESENT_ALLOW_TEARING.0 as u32
        } else {
            flags
        },
    )
}

fn log_present_result(
    entry_point: &str,
    result: i32,
    original_sync_interval: u32,
    forwarded_sync_interval: u32,
    forwarded_flags: u32,
) {
    if result == 0 {
        return;
    }
    static NONZERO_RESULTS: AtomicU64 = AtomicU64::new(0);
    let count = NONZERO_RESULTS.fetch_add(1, Ordering::Relaxed) + 1;
    if count != 1 && !count.is_multiple_of(120) {
        return;
    }
    let status = if result as u32 == 0x087a_0001 {
        " (DXGI_STATUS_OCCLUDED)"
    } else {
        ""
    };
    tracing::debug!(
        "snowglass: {entry_point} returned {:#010x}{status} (count={count}, sync {}->{}, flags={:#x})",
        result as u32,
        original_sync_interval,
        forwarded_sync_interval,
        forwarded_flags
    );
}

pub(crate) unsafe fn present(
    this: *mut c_void,
    sync_interval: u32,
    flags: u32,
    mut next: crate::vtable::PresentContext<'_>,
) -> i32 {
    unsafe { before_present(this, next.owner) };
    let unthrottled = snowglass_hook::config::present_unthrottled();
    let allow_tearing =
        unthrottled && *next.allow_tearing && unsafe { swapchain_is_windowed(this) };
    let (forwarded_sync, forwarded_flags) =
        forwarded_present_arguments(sync_interval, flags, unthrottled, allow_tearing);
    let result = snowglass_hook::next!(next, this, forwarded_sync, forwarded_flags);
    log_present_result(
        "Present",
        result,
        sync_interval,
        forwarded_sync,
        forwarded_flags,
    );
    result
}

pub(crate) unsafe fn present1(
    this: *mut c_void,
    sync_interval: u32,
    flags: u32,
    parameters: *const c_void,
    mut next: crate::vtable::Present1Context<'_>,
) -> i32 {
    unsafe { before_present(this, next.owner) };
    let unthrottled = snowglass_hook::config::present_unthrottled();
    let allow_tearing =
        unthrottled && *next.allow_tearing && unsafe { swapchain_is_windowed(this) };
    let (forwarded_sync, forwarded_flags) =
        forwarded_present_arguments(sync_interval, flags, unthrottled, allow_tearing);
    let result = snowglass_hook::next!(next, this, forwarded_sync, forwarded_flags, parameters);
    log_present_result(
        "Present1",
        result,
        sync_interval,
        forwarded_sync,
        forwarded_flags,
    );
    result
}

pub(crate) unsafe fn resize_buffers(
    this: *mut c_void,
    buffer_count: u32,
    width: u32,
    height: u32,
    format: u32,
    flags: u32,
    mut next: crate::vtable::ResizeBuffersContext<'_>,
) -> i32 {
    let owner = next.owner.clone();
    let allow_tearing = next.allow_tearing.clone();
    if let Some(owner) = &owner {
        guarded(this, "before_resize", || unsafe {
            owner.before_resize(this)
        });
    }
    let result = snowglass_hook::next!(next, this, buffer_count, width, height, format, flags);
    if result >= 0 {
        allow_tearing.store(
            flags & DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 as u32 != 0,
            Ordering::Relaxed,
        );
    }
    if let Some(owner) = &owner {
        guarded(this, "after_resize", || unsafe {
            owner.after_resize(this, result)
        });
    }
    result
}

pub(crate) unsafe fn swapchain_destroyed(this: *mut c_void, state: &dyn SwapchainHook) {
    let owner = state.state().owner.write().take();
    if let Some(owner) = owner {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
            owner.destroyed(this)
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn present_arguments_only_change_for_the_opt_in() {
        assert_eq!(forwarded_present_arguments(1, 4, false, true), (1, 4));
        assert_eq!(forwarded_present_arguments(1, 4, true, false), (0, 4));
        assert_eq!(
            forwarded_present_arguments(1, 4, true, true),
            (0, 4 | DXGI_PRESENT_ALLOW_TEARING.0 as u32)
        );
    }
}
