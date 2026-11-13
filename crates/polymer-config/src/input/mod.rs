//! Input projection and virtual-device identity used during serialization.

mod configuration;
pub mod gamepad;
pub mod projection;
mod source;

pub use configuration::*;
pub use source::{InputSource, PointerAxis};
