use polymer_platform_windows::NativeWindow;

/// Native window metadata collection, separate from computational assignment.
pub struct ProducerWindows;

impl ProducerWindows {
    /// Called after the fixed assignment commits. Keep the returned guard on
    /// the application-facing object until that binding ends, not on an IPC
    /// connection that can disconnect independently.
    pub fn bind_fixed(
        binding: &crate::swapchain::FixedSurfaceBinding,
    ) -> windows::core::Result<polymer_platform_windows::FixedWindowBinding> {
        polymer_platform_windows::FixedWindowBinding::new(NativeWindow(
            binding.window().handle as isize,
        ))
    }
}
