use std::ffi::c_void;

// Public WDK prefixes: d3d10umddi.h and dxgiddi.h. These are driver ABI
// structures, not offsets into the D3D runtime's private COM objects.
#[repr(C)]
pub(super) struct OpenAdapter {
    pub runtime: usize,
    pub driver: usize,
    pub interface: u32,
    pub version: u32,
    pub callbacks: usize,
    pub functions: *mut usize,
}
#[repr(C)]
pub(super) struct CreateDevice {
    pub runtime: usize,
    pub interface: u32,
    pub version: u32,
    pub kernel_callbacks: usize,
    pub functions: *mut usize,
    pub driver: usize,
    pub dxgi_callbacks: usize,
    pub dxgi_functions: *mut usize,
}
#[repr(C)]
pub(super) struct Rotate {
    pub device: usize,
    pub resources: *const usize,
    pub count: u32,
}
pub(super) type OpenFn = unsafe extern "system" fn(*mut OpenAdapter) -> i32;
pub(super) type DeviceFn = unsafe extern "system" fn(usize, *mut CreateDevice) -> i32;
pub(super) type CloseFn = unsafe extern "system" fn(usize) -> i32;
pub(super) type ResourceFn = unsafe extern "system" fn(usize, *const c_void, usize, usize);
pub(super) type RelocateFn = unsafe extern "system" fn(usize, *mut usize);
pub(super) type DestroyFn = unsafe extern "system" fn(usize);
pub(super) type RotateFn = unsafe extern "system" fn(*mut Rotate) -> i32;

// Identical public prefixes in D3D11, 11.1, WDDM 1.3, 2.0, 2.1, 2.2 and 2.6.
pub(super) const RESOURCE_CREATE: usize = 58;
pub(super) const RELOCATE: usize = 55;
pub(super) const DEVICE_DESTROY: usize = 99;
pub(super) const ROTATE: usize = 5;
