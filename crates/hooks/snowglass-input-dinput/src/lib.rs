//! `dinput8.dll` proxy and DirectInput device interception boundary.
#![allow(unsafe_code)]

mod hook_chain;
mod hooks;
mod loader;
mod runtime;
mod sys;
mod vtable;

pub use hooks::dispatch_direct_input_8_create;
pub use loader::{DirectInput8CreateFn, system_direct_input8_create};
pub use runtime::intercept_direct_input8_create;
pub use sys::*;

fn register_hooks() {
    static REGISTER: std::sync::Once = std::sync::Once::new();
    REGISTER.call_once(|| {
        hooks::register_direct_input_8_create(runtime::direct_input_8_create);
        hooks::register_create_device(runtime::create_device);
        hooks::register_get_device_state(runtime::get_device_state);
        hooks::register_get_device_data(runtime::get_device_data);
        hooks::register_set_event_notification(runtime::set_event_notification);
    });
}
