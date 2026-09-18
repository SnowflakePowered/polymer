mod bindings;
pub(crate) mod storage;
pub(crate) use bindings::{PlatformBinding, extension};
pub use storage::Device;

use crate::plugin::PluginInstance;
pub trait PlatformPluginInstance: PluginInstance {
    fn platforms(&self) -> &[String];
}
