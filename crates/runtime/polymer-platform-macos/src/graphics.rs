use polymer_protocol::GraphicsHook;

/// Graphics APIs Polymer captures on macOS, in preference order. Vulkan runs on MoltenVK.
pub struct GraphicsSupport;

impl GraphicsSupport {
    pub fn available() -> &'static [GraphicsHook] {
        &[GraphicsHook::Metal, GraphicsHook::Vulkan]
    }
}
