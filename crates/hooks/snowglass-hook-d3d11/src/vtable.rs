use std::ffi::c_void;
use std::sync::Arc;
use windows::{
    Win32::Graphics::Direct3D11::{
        ID3D11Device, ID3D11DeviceContext, ID3D11DeviceContext1, ID3D11DeviceContext2,
        ID3D11DeviceContext3, ID3D11DeviceContext4,
    },
    core::Interface,
};

snowglass_hook::vtable! {
    [
        ID3D11DeviceContext,
        ID3D11DeviceContext1,
        ID3D11DeviceContext2,
        ID3D11DeviceContext3,
        ID3D11DeviceContext4,
    ],
}

snowglass_hook::com_vtable! {
    union ID3D11DeviceContextVtable for ID3D11DeviceContext4;
}

pub(crate) struct ContextHookContext {
    pub(crate) tracking: Arc<snowglass_producer_d3d11::Tracking>,
}

snowglass_hook::impl_com_hook! {
    trait ContextHook;
    impl ContextHook for ID3D11DeviceContextHook {
        #![vtable = ID3D11DeviceContextVtable]
        #[interfaces] {
            ID3D11DeviceContext4,
            ID3D11DeviceContext3,
            ID3D11DeviceContext2,
            ID3D11DeviceContext1,
            ID3D11DeviceContext,
        }
        #[state = ContextHookContext]

        #[release]
        unsafe extern "system" fn context_release(this: *mut c_void) -> u32;

        #[base = ID3D11DeviceContext::Draw]
        #[postfix(|_result, context| unsafe { context.state.tracking.record_context_draw(this) })]
        unsafe extern "system" fn draw(this: *mut c_void, vertex_count: u32, start_vertex: u32);

        #[base = ID3D11DeviceContext::DrawIndexed]
        #[postfix(|_result, context| unsafe { context.state.tracking.record_context_draw(this) })]
        unsafe extern "system" fn draw_indexed(
            this: *mut c_void,
            index_count: u32,
            start_index: u32,
            base_vertex: i32,
        );

        #[base = ID3D11DeviceContext::DrawIndexedInstanced]
        #[postfix(|_result, context| unsafe { context.state.tracking.record_context_draw(this) })]
        unsafe extern "system" fn draw_indexed_instanced(
            this: *mut c_void,
            index_count_per_instance: u32,
            instance_count: u32,
            start_index: u32,
            base_vertex: i32,
            start_instance: u32,
        );

        #[base = ID3D11DeviceContext::DrawInstanced]
        #[postfix(|_result, context| unsafe { context.state.tracking.record_context_draw(this) })]
        unsafe extern "system" fn draw_instanced(
            this: *mut c_void,
            vertex_count_per_instance: u32,
            instance_count: u32,
            start_vertex: u32,
            start_instance: u32,
        );

        #[base = ID3D11DeviceContext::DrawAuto]
        #[postfix(|_result, context| unsafe { context.state.tracking.record_context_draw(this) })]
        unsafe extern "system" fn draw_auto(this: *mut c_void);

        #[base = ID3D11DeviceContext::DrawIndexedInstancedIndirect]
        #[postfix(|_result, context| unsafe { context.state.tracking.record_context_draw(this) })]
        unsafe extern "system" fn draw_indexed_instanced_indirect(
            this: *mut c_void,
            argument_buffer: *mut c_void,
            argument_offset: u32,
        );

        #[base = ID3D11DeviceContext::DrawInstancedIndirect]
        #[postfix(|_result, context| unsafe { context.state.tracking.record_context_draw(this) })]
        unsafe extern "system" fn draw_instanced_indirect(
            this: *mut c_void,
            argument_buffer: *mut c_void,
            argument_offset: u32,
        );
    }
}

impl Clone for ContextHookContext {
    fn clone(&self) -> Self {
        Self {
            tracking: self.tracking.clone(),
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
    if unsafe { ID3D11DeviceContextHook::is_installed(context) } {
        return;
    }
    unsafe { ID3D11DeviceContextHook::install_widest(context, ContextHookContext { tracking }) };
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
    ID3D11DeviceContextHook::with_state(context.as_raw(), |state| state.tracking.clone())
}
