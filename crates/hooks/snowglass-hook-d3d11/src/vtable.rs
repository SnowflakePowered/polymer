use dashmap::DashMap;
use std::ffi::c_void;
use std::sync::{Arc, LazyLock};
use windows::{
    Win32::Graphics::Direct3D11::{
        ID3D11Device, ID3D11DeviceContext, ID3D11DeviceContext_Vtbl, ID3D11DeviceContext1,
        ID3D11DeviceContext1_Vtbl, ID3D11DeviceContext2, ID3D11DeviceContext2_Vtbl,
        ID3D11DeviceContext3, ID3D11DeviceContext3_Vtbl, ID3D11DeviceContext4,
        ID3D11DeviceContext4_Vtbl,
    },
    core::{IUnknown, Interface},
};

snowglass_hook::vtable! {
    [
        ID3D11DeviceContext_Vtbl,
        ID3D11DeviceContext1_Vtbl,
        ID3D11DeviceContext2_Vtbl,
        ID3D11DeviceContext3_Vtbl,
        ID3D11DeviceContext4_Vtbl,
    ],
}

type DrawIndexedFn = unsafe extern "system" fn(*mut c_void, u32, u32, i32);
type DrawFn = unsafe extern "system" fn(*mut c_void, u32, u32);
type InstallFn = unsafe fn(*mut c_void, Arc<snowglass_producer_d3d11::Tracking>) -> bool;

snowglass_hook::com_vtable! {
    union ID3D11DeviceContextVtable for ID3D11DeviceContext4_Vtbl;
}

/// The layout-erased view of a hooked immediate context stored in the
/// registry. `Draw`/`DrawIndexed` live in the base `ID3D11DeviceContext`
/// layout common to every generation, so the exposed continuations do not
/// depend on the cloned width.
trait ContextHook: snowglass_hook::VtableHook {
    fn tracking(&self) -> &Arc<snowglass_producer_d3d11::Tracking>;
    fn draw(&self) -> DrawFn;
    fn draw_indexed(&self) -> DrawIndexedFn;
}

snowglass_hook::impl_com_hook! {
    impl ContextHook for ID3D11DeviceContextHook {
        vtable ID3D11DeviceContextVtable;
        context {
            /// Device-scoped tracking shared with the swapchains this device presents.
            tracking: Arc<snowglass_producer_d3d11::Tracking>,
        };
        patch |patched| {
        // SAFETY: the slots written below were copied above, so each place
        // projection targets an initialized entry within the clone.
        unsafe {
            let table = patched.as_mut_ptr::<ID3D11DeviceContext_Vtbl>();
            (*table).Draw = draw_trampoline;
            (*table).DrawIndexed = draw_indexed_trampoline;
        }
        }

        #[vtable = ID3D11DeviceContext_Vtbl]
        #[base = Draw]
        fn draw(&self) -> DrawFn;
        #[vtable = ID3D11DeviceContext_Vtbl]
        #[base = DrawIndexed]
        fn draw_indexed(&self) -> DrawIndexedFn;
    }
}

static CONTEXTS: LazyLock<DashMap<usize, Box<dyn ContextHook>>> = LazyLock::new(Default::default);

/// Installs draw interception on a device's immediate context so the producer
/// can track render regions.
///
/// # Safety
///
/// `output` must be null or point to a COM interface pointer for a live
/// `ID3D11DeviceContext`.
pub(crate) unsafe fn install_immediate_context(
    output: *mut *mut c_void,
    tracking: Arc<snowglass_producer_d3d11::Tracking>,
) {
    if !snowglass_hook::config::tracking_enabled() || output.is_null() {
        return;
    }
    let context = unsafe { *output };
    if context.is_null() {
        return;
    }
    let current = unsafe { *context.cast::<*const usize>() };
    if current.is_null() {
        return;
    }
    if CONTEXTS
        .get(&(context as usize))
        .is_some_and(|state| state.patched_vtable() == current)
    {
        return;
    }
    let Some(install) = widest_context(context) else {
        return;
    };
    unsafe { install(context, tracking) };
}

/// Resolves the tracking context installed on a D3D11 device's immediate
/// context.
///
/// # Safety
///
/// `device` must be null or point to a live `ID3D11Device`.
pub(crate) unsafe fn device_tracking(
    device: *mut c_void,
) -> Option<Arc<snowglass_producer_d3d11::Tracking>> {
    let device = unsafe { ID3D11Device::from_raw_borrowed(&device) }?;
    let context = unsafe { device.GetImmediateContext() }.ok()?;
    CONTEXTS
        .get(&(context.as_raw() as usize))
        .map(|state| state.tracking().clone())
}

fn widest_context(context: *mut c_void) -> Option<InstallFn> {
    let unknown = unsafe { IUnknown::from_raw_borrowed(&context) }?;
    // The immediate context does not tear off, so every generation shares one
    // pointer; only the widest supported vtable width matters, and it is
    // installed into the object's original pointer by the caller.
    snowglass_hook::widest_interfaces! {
        from unknown => InstallFn;
        install install_context_kind;
        ID3D11DeviceContext4 => ID3D11DeviceContext4_Vtbl,
        ID3D11DeviceContext3 => ID3D11DeviceContext3_Vtbl,
        ID3D11DeviceContext2 => ID3D11DeviceContext2_Vtbl,
        ID3D11DeviceContext1 => ID3D11DeviceContext1_Vtbl,
        ID3D11DeviceContext => ID3D11DeviceContext_Vtbl,
    }
    .into_iter()
    .next()
    .map(|(_, install)| install)
}

unsafe fn install_context_kind<A: Vtable>(
    context: *mut c_void,
    tracking: Arc<snowglass_producer_d3d11::Tracking>,
) -> bool {
    if context.is_null() {
        return false;
    }
    let object_vtable = context.cast::<*const usize>();
    let current = unsafe { *object_vtable };
    let Some(hook) = (unsafe { ID3D11DeviceContextHook::<A>::clone_from(current, tracking) })
    else {
        return false;
    };
    let state: Box<dyn ContextHook> = Box::new(hook);
    unsafe { *object_vtable = state.patched_vtable() };
    CONTEXTS.insert(context as usize, state);
    true
}

fn context_tracking(context: *mut c_void) -> Option<Arc<snowglass_producer_d3d11::Tracking>> {
    CONTEXTS
        .get(&(context as usize))
        .map(|state| state.tracking().clone())
}

snowglass_hook::com_trampoline! {
    unsafe extern "system" fn draw_trampoline(
        this: *mut c_void,
        vertex_count: u32,
        start_vertex: u32,
    );
    original CONTEXTS.get(&(this as usize)).map(|state| state.draw());
    after {
        if let Some(tracking) = context_tracking(this) {
            unsafe { tracking.record_context_draw(this) };
        }
    }
}

snowglass_hook::com_trampoline! {
    unsafe extern "system" fn draw_indexed_trampoline(
        this: *mut c_void,
        index_count: u32,
        start_index: u32,
        base_vertex: i32,
    );
    original CONTEXTS.get(&(this as usize)).map(|state| state.draw_indexed());
    after {
        if let Some(tracking) = context_tracking(this) {
            unsafe { tracking.record_context_draw(this) };
        }
    }
}
