use std::ffi::c_void;
use std::sync::Arc;
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

type InstallFn = unsafe fn(*mut c_void, Arc<snowglass_producer_d3d11::Tracking>) -> bool;

snowglass_hook::com_vtable! {
    union ID3D11DeviceContextVtable for ID3D11DeviceContext4_Vtbl;
}

snowglass_hook::impl_com_hook! {
    pub(crate) registry CONTEXTS;
    trait ContextHook;
    impl ContextHook for ID3D11DeviceContextHook {
        vtable ID3D11DeviceContextVtable;
        #[release]
        unsafe extern "system" fn context_release(this: *mut c_void) -> u32 {
            context struct {
                /// Device-scoped tracking shared with the swapchains this device presents.
                tracking: Arc<snowglass_producer_d3d11::Tracking>,
            }
        }

        #[vtable = ID3D11DeviceContext_Vtbl]
        #[base = Draw]
        #[hooks(record_draw)]
        unsafe extern "system" fn draw(this: *mut c_void, vertex_count: u32, start_vertex: u32) {
            context |state| {
                tracking: Arc<snowglass_producer_d3d11::Tracking> = state.tracking().clone(),
            }
        }

        #[vtable = ID3D11DeviceContext_Vtbl]
        #[base = DrawIndexed]
        #[hooks(record_draw_indexed)]
        unsafe extern "system" fn draw_indexed(
            this: *mut c_void,
            index_count: u32,
            start_index: u32,
            base_vertex: i32,
        ) {
            context |state| {
                tracking: Arc<snowglass_producer_d3d11::Tracking> = state.tracking().clone(),
            }
        }

        #[vtable = ID3D11DeviceContext_Vtbl]
        #[base = DrawIndexedInstanced]
        #[hooks(record_draw_indexed_instanced)]
        unsafe extern "system" fn draw_indexed_instanced(
            this: *mut c_void,
            index_count_per_instance: u32,
            instance_count: u32,
            start_index: u32,
            base_vertex: i32,
            start_instance: u32,
        ) {
            context |state| {
                tracking: Arc<snowglass_producer_d3d11::Tracking> = state.tracking().clone(),
            }
        }

        #[vtable = ID3D11DeviceContext_Vtbl]
        #[base = DrawInstanced]
        #[hooks(record_draw_instanced)]
        unsafe extern "system" fn draw_instanced(
            this: *mut c_void,
            vertex_count_per_instance: u32,
            instance_count: u32,
            start_vertex: u32,
            start_instance: u32,
        ) {
            context |state| {
                tracking: Arc<snowglass_producer_d3d11::Tracking> = state.tracking().clone(),
            }
        }

        #[vtable = ID3D11DeviceContext_Vtbl]
        #[base = DrawAuto]
        #[hooks(record_draw_auto)]
        unsafe extern "system" fn draw_auto(this: *mut c_void) {
            context |state| {
                tracking: Arc<snowglass_producer_d3d11::Tracking> = state.tracking().clone(),
            }
        }

        #[vtable = ID3D11DeviceContext_Vtbl]
        #[base = DrawIndexedInstancedIndirect]
        #[hooks(record_draw_indexed_instanced_indirect)]
        unsafe extern "system" fn draw_indexed_instanced_indirect(
            this: *mut c_void,
            argument_buffer: *mut c_void,
            argument_offset: u32,
        ) {
            context |state| {
                tracking: Arc<snowglass_producer_d3d11::Tracking> = state.tracking().clone(),
            }
        }

        #[vtable = ID3D11DeviceContext_Vtbl]
        #[base = DrawInstancedIndirect]
        #[hooks(record_draw_instanced_indirect)]
        unsafe extern "system" fn draw_instanced_indirect(
            this: *mut c_void,
            argument_buffer: *mut c_void,
            argument_offset: u32,
        ) {
            context |state| {
                tracking: Arc<snowglass_producer_d3d11::Tracking> = state.tracking().clone(),
            }
        }
    }
}

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
    unsafe { ID3D11DeviceContextHook::<A>::install(context, tracking) }
}

macro_rules! record_context_draw_hook {
    ($hook:ident, $context:ident($($argument:ident: $argument_type:ty),* $(,)?)) => {
        unsafe fn $hook(
            this: *mut c_void,
            $($argument: $argument_type,)*
            mut next: $context<'_>,
        ) {
            let tracking = next.tracking.clone();
            snowglass_hook::next!(next, this, $($argument),*);
            unsafe { tracking.record_context_draw(this) };
        }
    };
}

record_context_draw_hook!(record_draw, DrawContext(
    vertex_count: u32,
    start_vertex: u32,
));
record_context_draw_hook!(record_draw_indexed, DrawIndexedContext(
    index_count: u32,
    start_index: u32,
    base_vertex: i32,
));
record_context_draw_hook!(record_draw_indexed_instanced, DrawIndexedInstancedContext(
    index_count_per_instance: u32,
    instance_count: u32,
    start_index: u32,
    base_vertex: i32,
    start_instance: u32,
));
record_context_draw_hook!(record_draw_instanced, DrawInstancedContext(
    vertex_count_per_instance: u32,
    instance_count: u32,
    start_vertex: u32,
    start_instance: u32,
));
record_context_draw_hook!(record_draw_auto, DrawAutoContext());
record_context_draw_hook!(record_draw_indexed_instanced_indirect, DrawIndexedInstancedIndirectContext(
    argument_buffer: *mut c_void,
    argument_offset: u32,
));
record_context_draw_hook!(record_draw_instanced_indirect, DrawInstancedIndirectContext(
    argument_buffer: *mut c_void,
    argument_offset: u32,
));
