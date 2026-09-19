mod bindings;
pub(crate) use bindings::{PlatformBinding, extension};

use crate::plugin::PluginInstance;
pub trait PlatformPluginInstance: PluginInstance {
    fn platforms(&self) -> &[String];
}
