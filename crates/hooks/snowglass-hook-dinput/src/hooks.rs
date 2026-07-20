//! Concrete DirectInput hook-chain declarations.

use crate::{
    DirectInput8CreateFn,
    hook_chain::{__resolve_observer_terminal, chain, post_chain},
};
use std::ffi::c_void;
use windows_sys::{
    Win32::Foundation::HINSTANCE,
    core::{GUID, HRESULT},
};

chain! {
    pub dispatch_direct_input_8_create, register_direct_input_8_create;
    after DirectInput8Create;
    chain DIRECT_INPUT_8_CREATE_CHAIN, context DirectInput8CreateContext {
        runtime: std::sync::Arc<crate::runtime::Runtime> = crate::install::runtime(),
    };
    hook DirectInput8CreateHook(instance: HINSTANCE, version: u32, interface_id: *const GUID, output: *mut *mut c_void, outer: *mut c_void) -> result: HRESULT;
    abi (original: DirectInput8CreateFn, instance: HINSTANCE, version: u32, interface_id: *const GUID, output: *mut *mut c_void, outer: *mut c_void);
    resolve with original: DirectInput8CreateFn;
}
