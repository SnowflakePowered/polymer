//! Route the combined D3D11 export through the same DXGI creation boundary.
use std::ffi::c_void;
use windows::{
    Win32::{
        Foundation::E_FAIL,
        Graphics::{Direct3D11::*, Dxgi::*},
    },
    core::Interface,
};

pub(crate) struct VirtualCreation;
impl VirtualCreation {
    #[allow(clippy::too_many_arguments)] // Native export ABI.
    pub(crate) unsafe fn create(
        adapter: *mut c_void,
        driver_type: i32,
        software: windows_sys::Win32::Foundation::HMODULE,
        flags: u32,
        levels: *const i32,
        level_count: u32,
        sdk: u32,
        desc: *const c_void,
        swapchain: *mut *mut c_void,
        device: *mut *mut c_void,
        level: *mut i32,
        context: *mut *mut c_void,
        mut next: crate::hooks::CreateDeviceAndSwapchainContext<'_>,
    ) -> i32 {
        if swapchain.is_null() || desc.is_null() {
            return polymer_hook::next!(
                next,
                adapter,
                driver_type,
                software,
                flags,
                levels,
                level_count,
                sdk,
                desc,
                swapchain,
                device,
                level,
                context
            );
        }
        unsafe {
            swapchain.write(std::ptr::null_mut());
            if !device.is_null() {
                device.write(std::ptr::null_mut());
            }
            if !context.is_null() {
                context.write(std::ptr::null_mut());
            }
        }
        let mut raw_device = std::ptr::null_mut();
        let mut raw_context = std::ptr::null_mut();
        let result = unsafe {
            crate::install::system_create_device(
                adapter,
                driver_type,
                software,
                flags,
                levels,
                level_count,
                sdk,
                &mut raw_device,
                level,
                &mut raw_context,
            )
        };
        if result < 0 {
            return result;
        }
        if raw_device.is_null() {
            return E_FAIL.0;
        }
        let native_device = unsafe { ID3D11Device::from_raw(raw_device) };
        let native_context =
            (!raw_context.is_null()).then(|| unsafe { ID3D11DeviceContext::from_raw(raw_context) });
        let result = (|| -> windows::core::Result<IDXGISwapChain> {
            let adapter = unsafe { native_device.cast::<IDXGIDevice>()?.GetAdapter()? };
            let factory: IDXGIFactory = unsafe { adapter.GetParent()? };
            if !unsafe { polymer_hook_dxgi::install_factory_interfaces(factory.as_raw()) } {
                return Err(windows::core::Error::from_hresult(E_FAIL));
            }
            let description = unsafe { *desc.cast::<DXGI_SWAP_CHAIN_DESC>() };
            let mut created = None;
            unsafe {
                factory
                    .CreateSwapChain(&native_device, &description, &mut created)
                    .ok()?;
            }
            created.ok_or_else(|| windows::core::Error::from_hresult(E_FAIL))
        })();
        match result {
            Ok(created) => {
                unsafe {
                    swapchain.write(created.into_raw());
                    if !device.is_null() {
                        device.write(native_device.into_raw());
                    }
                    if !context.is_null() {
                        context.write(
                            native_context.map_or(std::ptr::null_mut(), Interface::into_raw),
                        );
                    }
                }
                0
            }
            Err(error) => error.code().0,
        }
    }
}
