use crate::registry::PresentationRegistry;
use polymer_platform_windows::{NativeWindow, ProcessId, is_process_window};

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

    /// Validate observed process windows on the presenting thread. A zero
    /// handle denotes an unspecified window, not evidence of destruction.
    /// Native resources are retired separately under their backend's rules.
    pub fn refresh(registry: &PresentationRegistry) {
        let process = ProcessId(std::process::id());
        let destroyed = registry
            .observed_windows()
            .into_iter()
            .filter(|window| *window != 0 && !is_process_window(NativeWindow(*window), process))
            .collect::<Vec<_>>();
        for window in destroyed {
            registry.forget_window(window);
        }
    }
}
