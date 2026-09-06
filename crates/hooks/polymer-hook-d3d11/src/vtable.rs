use std::ffi::c_void;
use std::sync::Arc;
use windows::{
    Win32::Graphics::Direct3D11::{
        ID3D11CommandList, ID3D11Device, ID3D11Device1, ID3D11Device2, ID3D11Device3,
        ID3D11Device4, ID3D11Device5, ID3D11DeviceContext, ID3D11DeviceContext1,
        ID3D11DeviceContext2, ID3D11DeviceContext3, ID3D11DeviceContext4,
    },
    core::Interface,
};

polymer_hook::vtable! {
    [ID3D11Device, ID3D11Device1, ID3D11Device2, ID3D11Device3, ID3D11Device4, ID3D11Device5],
    [ID3D11CommandList],
    [
        ID3D11DeviceContext,
        ID3D11DeviceContext1,
        ID3D11DeviceContext2,
        ID3D11DeviceContext3,
        ID3D11DeviceContext4,
    ],
}

polymer_hook::com_vtable! {
    union ID3D11DeviceContextVtable for ID3D11DeviceContext4;
}

pub(crate) struct ContextHookContext {
    pub(crate) tracking: Arc<polymer_producer_d3d11::Tracking>,
    lifetime: Option<Arc<ContextLifetime>>,
}

struct ContextLifetime {
    tracking: Arc<polymer_producer_d3d11::Tracking>,
    id: usize,
}
impl Drop for ContextLifetime {
    fn drop(&mut self) {
        self.tracking.forget_context(self.id);
    }
}
impl ContextHookContext {
    fn deferred(tracking: Arc<polymer_producer_d3d11::Tracking>, id: usize) -> Self {
        Self {
            lifetime: Some(Arc::new(ContextLifetime {
                tracking: tracking.clone(),
                id,
            })),
            tracking,
        }
    }
}

polymer_hook::impl_com_hook! {
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

        #[base = ID3D11DeviceContext::PSSetShaderResources]
        #[postfix(|_result, context| unsafe { context.state.tracking.shader_resources(this, start, count, views) })]
        unsafe extern "system" fn set_shader_resources(this: *mut c_void, start: u32, count: u32, views: *const *mut c_void);

        #[base = ID3D11DeviceContext::OMSetRenderTargets]
        #[postfix(|_result, context| unsafe { context.state.tracking.render_targets(this, count, views) })]
        unsafe extern "system" fn set_render_targets(this: *mut c_void, count: u32, views: *const *mut c_void, depth: *mut c_void);

        #[base = ID3D11DeviceContext::RSSetViewports]
        #[postfix(|_result, context| unsafe { context.state.tracking.viewport(this, count, value) })]
        unsafe extern "system" fn set_viewport(this: *mut c_void, count: u32, value: *const windows::Win32::Graphics::Direct3D11::D3D11_VIEWPORT);

        #[base = ID3D11DeviceContext::RSSetScissorRects]
        #[postfix(|_result, context| unsafe { context.state.tracking.scissor(this, count, value) })]
        unsafe extern "system" fn set_scissor(this: *mut c_void, count: u32, value: *const windows::Win32::Foundation::RECT);

        #[base = ID3D11DeviceContext::RSSetState]
        #[postfix(|_result, context| unsafe { context.state.tracking.rasterizer(this, value) })]
        unsafe extern "system" fn set_rasterizer(this: *mut c_void, value: *mut c_void);

        #[base = ID3D11DeviceContext::ClearState]
        #[postfix(|_result, context| context.state.tracking.clear_context(this))]
        unsafe extern "system" fn clear_state(this: *mut c_void);

        #[base = ID3D11DeviceContext::ClearRenderTargetView]
        #[postfix(|_result, context| unsafe { context.state.tracking.record_clear(this, view) })]
        unsafe extern "system" fn clear_render_target(this: *mut c_void, view: *mut c_void, color: *const f32);

        #[base = ID3D11DeviceContext::FinishCommandList]
        #[fallback = windows::Win32::Foundation::E_FAIL]
        #[postfix(|result, context| unsafe {
            if result.is_ok() && !output.is_null() && !(*output).is_null() {
                context.state.tracking.finish_commands(this, *output, restore.as_bool());
                ID3D11CommandListHook::install_all_interfaces(*output, ListHookContext(Arc::new(ListLifetime { tracking: context.state.tracking.clone(), id: *output as usize })));
            }
        })]
        unsafe extern "system" fn finish_command_list(this: *mut c_void, restore: windows::core::BOOL, output: *mut *mut c_void) -> windows::core::HRESULT;

        #[base = ID3D11DeviceContext::ExecuteCommandList]
        #[infix(track_list_execution)]
        unsafe extern "system" fn execute_command_list(this: *mut c_void, list: *mut c_void, restore: windows::core::BOOL);

        #[base = ID3D11DeviceContext::CopyResource]
        #[postfix(|_result, context| unsafe { context.state.tracking.record_copy(this, destination, 0, 0, 0, source, 0, std::ptr::null(), false) })]
        unsafe extern "system" fn copy_resource(this: *mut c_void, destination: *mut c_void, source: *mut c_void);

        #[base = ID3D11DeviceContext::CopySubresourceRegion]
        #[postfix(|_result, context| unsafe { context.state.tracking.record_copy(this, destination, destination_subresource, x, y, source, source_subresource, source_box, false) })]
        unsafe extern "system" fn copy_subresource_region(this: *mut c_void, destination: *mut c_void, destination_subresource: u32, x: u32, y: u32, z: u32,
            source: *mut c_void, source_subresource: u32, source_box: *const windows::Win32::Graphics::Direct3D11::D3D11_BOX);

        #[base = ID3D11DeviceContext::ResolveSubresource]
        #[postfix(|_result, context| unsafe { context.state.tracking.record_copy(this, destination, destination_subresource, 0, 0, source, source_subresource, std::ptr::null(), true) })]
        unsafe extern "system" fn resolve_subresource(this: *mut c_void, destination: *mut c_void, destination_subresource: u32, source: *mut c_void, source_subresource: u32,
            format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT);

        #[base = ID3D11DeviceContext::Draw]
        #[postfix(|_result, context| unsafe {
            context.state.tracking.record_context_draw(
                this,
                Some((vertex_count, start_vertex)),
            )
        })]
        unsafe extern "system" fn draw(this: *mut c_void, vertex_count: u32, start_vertex: u32);

        #[base = ID3D11DeviceContext::DrawIndexed]
        #[postfix(|_result, context| unsafe {
            context.state.tracking.record_context_draw(this, None)
        })]
        unsafe extern "system" fn draw_indexed(
            this: *mut c_void,
            index_count: u32,
            start_index: u32,
            base_vertex: i32,
        );

        #[base = ID3D11DeviceContext::DrawIndexedInstanced]
        #[postfix(|_result, context| unsafe {
            context.state.tracking.record_context_draw(this, None)
        })]
        unsafe extern "system" fn draw_indexed_instanced(
            this: *mut c_void,
            index_count_per_instance: u32,
            instance_count: u32,
            start_index: u32,
            base_vertex: i32,
            start_instance: u32,
        );

        #[base = ID3D11DeviceContext::DrawInstanced]
        #[postfix(|_result, context| unsafe {
            context.state.tracking.record_context_draw(
                this,
                (instance_count == 1).then_some((vertex_count_per_instance, start_vertex)),
            )
        })]
        unsafe extern "system" fn draw_instanced(
            this: *mut c_void,
            vertex_count_per_instance: u32,
            instance_count: u32,
            start_vertex: u32,
            start_instance: u32,
        );

        #[base = ID3D11DeviceContext::DrawAuto]
        #[postfix(|_result, context| unsafe {
            context.state.tracking.record_context_draw(this, None)
        })]
        unsafe extern "system" fn draw_auto(this: *mut c_void);

        #[base = ID3D11DeviceContext::DrawIndexedInstancedIndirect]
        #[postfix(|_result, context| unsafe {
            context.state.tracking.record_context_draw(this, None)
        })]
        unsafe extern "system" fn draw_indexed_instanced_indirect(
            this: *mut c_void,
            argument_buffer: *mut c_void,
            argument_offset: u32,
        );

        #[base = ID3D11DeviceContext::DrawInstancedIndirect]
        #[postfix(|_result, context| unsafe {
            context.state.tracking.record_context_draw(this, None)
        })]
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
            lifetime: self.lifetime.clone(),
        }
    }
}

/// Installs draw interception on a device's immediate context so the producer
/// can track render regions.
///
/// # Safety
///
/// `context` must be null or point to a live `ID3D11DeviceContext`.
pub(crate) unsafe fn install_immediate_context(
    context: *mut c_void,
    tracking: Arc<polymer_producer_d3d11::Tracking>,
) {
    if !polymer_hook::config::tracking_enabled() || context.is_null() {
        return;
    }
    let current = unsafe { *context.cast::<*const usize>() };
    if current.is_null() {
        return;
    }
    if unsafe { ID3D11DeviceContextHook::is_installed(context) } {
        return;
    }
    if let Some(context_ref) = unsafe { ID3D11DeviceContext::from_raw_borrowed(&context) }
        && let Ok(device) = unsafe { context_ref.GetDevice() }
    {
        unsafe {
            ID3D11DeviceHook::install_all_interfaces(
                device.as_raw(),
                ContextHookContext {
                    tracking: tracking.clone(),
                    lifetime: None,
                },
            )
        };
    }
    unsafe {
        ID3D11DeviceContextHook::install_widest(
            context,
            ContextHookContext {
                tracking,
                lifetime: None,
            },
        )
    };
}

/// Resolves the tracking context installed on a D3D11 device's immediate
/// context.
///
/// # Safety
///
/// `device` must be null or point to a live `ID3D11Device`.
pub(crate) unsafe fn device_tracking(
    device: *mut c_void,
) -> Option<Arc<polymer_producer_d3d11::Tracking>> {
    let device = unsafe { ID3D11Device::from_raw_borrowed(&device) }?;
    let context = unsafe { device.GetImmediateContext() }.ok()?;
    ID3D11DeviceContextHook::with_state(context.as_raw(), |state| state.tracking.clone())
}

polymer_hook::com_vtable! {
    union ID3D11DeviceVtable for ID3D11Device5;
}
polymer_hook::com_vtable! {
    union ID3D11CommandListVtable for ID3D11CommandList;
}

#[derive(Clone)]
struct ListHookContext(Arc<ListLifetime>);
struct ListLifetime {
    tracking: Arc<polymer_producer_d3d11::Tracking>,
    id: usize,
}
impl Drop for ListLifetime {
    fn drop(&mut self) {
        self.tracking.forget_commands(self.id);
    }
}

polymer_hook::impl_com_hook! {
    trait ListHook;
    impl ListHook for ID3D11CommandListHook {
        #![vtable = ID3D11CommandListVtable]
        #[interfaces] { ID3D11CommandList }
        #[state = ListHookContext]
        #[release]
        unsafe extern "system" fn list_release(this: *mut c_void) -> u32;
        #[base = ID3D11CommandList::GetContextFlags]
        #[fallback = 0]
        #[postfix(|_result, context| { let _ = &context.state.0; })]
        unsafe extern "system" fn get_context_flags(this: *mut c_void) -> u32;
    }
}

polymer_hook::impl_com_hook! {
    trait DeviceHook;
    impl DeviceHook for ID3D11DeviceHook {
        #![vtable = ID3D11DeviceVtable]
        #[interfaces] { ID3D11Device5, ID3D11Device4, ID3D11Device3, ID3D11Device2, ID3D11Device1, ID3D11Device }
        #[state = ContextHookContext]
        #[release]
        unsafe extern "system" fn device_release(this: *mut c_void) -> u32;
        #[base = ID3D11Device::CreateDeferredContext]
        #[fallback = windows::Win32::Foundation::E_FAIL]
        #[postfix(|result, context| unsafe {
            if result.is_ok() && !output.is_null() && !(*output).is_null() {
                context.state.tracking.deferred_context_created(*output);
                ID3D11DeviceContextHook::install_all_interfaces(*output, ContextHookContext::deferred(context.state.tracking.clone(), *output as usize));
            }
        })]
        unsafe extern "system" fn create_deferred(this: *mut c_void, flags: u32, output: *mut *mut c_void) -> windows::core::HRESULT;
        #[base = ID3D11Device1::CreateDeferredContext1]
        #[fallback = windows::Win32::Foundation::E_FAIL]
        #[postfix(|result, context| unsafe {
            if result.is_ok() && !output.is_null() && !(*output).is_null() {
                context.state.tracking.deferred_context_created(*output);
                ID3D11DeviceContextHook::install_all_interfaces(*output, ContextHookContext::deferred(context.state.tracking.clone(), *output as usize));
            }
        })]
        unsafe extern "system" fn create_deferred1(this: *mut c_void, flags: u32, output: *mut *mut c_void) -> windows::core::HRESULT;
        #[base = ID3D11Device2::CreateDeferredContext2]
        #[fallback = windows::Win32::Foundation::E_FAIL]
        #[postfix(|result, context| unsafe {
            if result.is_ok() && !output.is_null() && !(*output).is_null() {
                context.state.tracking.deferred_context_created(*output);
                ID3D11DeviceContextHook::install_all_interfaces(*output, ContextHookContext::deferred(context.state.tracking.clone(), *output as usize));
            }
        })]
        unsafe extern "system" fn create_deferred2(this: *mut c_void, flags: u32, output: *mut *mut c_void) -> windows::core::HRESULT;
        #[base = ID3D11Device3::CreateDeferredContext3]
        #[fallback = windows::Win32::Foundation::E_FAIL]
        #[postfix(|result, context| unsafe {
            if result.is_ok() && !output.is_null() && !(*output).is_null() {
                context.state.tracking.deferred_context_created(*output);
                ID3D11DeviceContextHook::install_all_interfaces(*output, ContextHookContext::deferred(context.state.tracking.clone(), *output as usize));
            }
        })]
        unsafe extern "system" fn create_deferred3(this: *mut c_void, flags: u32, output: *mut *mut c_void) -> windows::core::HRESULT;
    }
}
unsafe fn track_list_execution(
    this: *mut c_void,
    list: *mut c_void,
    restore: windows::core::BOOL,
    mut next: ExecuteCommandListContext<'_>,
) {
    let tracking = next.state.tracking.clone();
    tracking.execute_commands(list, || polymer_hook::next!(next, this, list, restore));
}
