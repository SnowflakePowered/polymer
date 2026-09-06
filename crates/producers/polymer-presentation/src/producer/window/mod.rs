use crate::registry::PresentationRegistry;
use polymer_platform_windows::{NativeWindow, ProcessId, is_process_window};

/// Native window metadata collection, separate from computational assignment.
pub struct ProducerWindows;

impl ProducerWindows {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Extent2D,
        registry::{RegistrySource, SourceIdentity},
    };
    use windows::{
        Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, WINDOW_EX_STYLE, WINDOW_STYLE,
        },
        core::w,
    };

    #[test]
    fn native_window_refresh_releases_destroyed_owner_without_disturbing_siblings() {
        let create = || unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("Polymer registry fixture"),
                WINDOW_STYLE(0),
                0,
                0,
                32,
                32,
                None,
                None,
                None,
                None,
            )
            .unwrap()
        };
        let destroyed = create();
        let live = create();
        let registry = PresentationRegistry::default();
        let source = |resource| RegistrySource {
            logical_slot: 0,
            identity: SourceIdentity {
                domain: 1,
                resource,
                subresource: 0,
            },
            extent: Extent2D {
                width: 32,
                height: 32,
            },
            placement: None,
        };
        registry.observe(1, destroyed.0 as isize, vec![source(1)], 1);
        registry.observe(2, live.0 as isize, vec![source(2)], 1);
        registry.observe(3, 0, vec![source(3)], 1);
        assert!(registry.claim(1, &[source(1).identity], 1));
        assert!(registry.claim(2, &[source(2).identity], 1));
        unsafe { DestroyWindow(destroyed).unwrap() };
        ProducerWindows::refresh(&registry);
        let observed = registry.observed_windows();
        let released = registry.claim(4, &[source(1).identity], 2);
        let sibling_released = registry.claim(4, &[source(2).identity], 2);
        unsafe { DestroyWindow(live).unwrap() };
        assert!(!observed.contains(&(destroyed.0 as isize)));
        assert!(observed.contains(&(live.0 as isize)));
        assert!(observed.contains(&0));
        assert!(released);
        assert!(!sibling_released);
        ProducerWindows::refresh(&registry);
        assert_eq!(registry.observed_windows(), vec![0]);
    }
}
