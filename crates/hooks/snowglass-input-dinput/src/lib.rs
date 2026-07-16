//! `dinput8.dll` proxy and DirectInput device interception boundary.
#![allow(unsafe_code)]

mod runtime;
mod sys;

pub use runtime::{
    DirectInput8CreateFn, intercept_direct_input8_create, system_direct_input8_create,
};
pub use sys::*;
