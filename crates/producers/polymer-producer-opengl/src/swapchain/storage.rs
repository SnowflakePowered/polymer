use super::*;
use polymer_presentation::registry::PresentationRegistry;

#[derive(Default)]
pub(super) struct CaptureStore {
    pub(super) entries: dashmap::DashMap<(usize, usize), Mutex<WindowCaptures>>,
    patterns: OnceLock<Vec<Option<regex::Regex>>>,
}
impl CaptureStore {
    pub(super) unsafe fn present(
        &self,
        hdc: usize,
        context: usize,
        hwnd: isize,
        extent: polymer_presentation::Extent2D,
    ) {
        let polymer_presentation::Extent2D { width, height } = extent;
        let now = SWAP_CLOCK.fetch_add(1, Ordering::Relaxed) + 1;
        unsafe { self.retire_idle(context, now) };
        let key = (hdc, context);
        {
            let stored = self.entries.entry(key).or_default();
            let mut entry = stored.lock();
            if entry.first_swap == 0 {
                entry.first_swap = now;
            }
            entry.hwnd = hwnd;
            entry.last_swap = now;
        }
        let selected = self.selected_windows(now);
        // GL resources may only be destroyed under their original context.
        for stored in self.entries.iter().filter(|entry| entry.key().1 == context) {
            let mut entry = stored.lock();
            let surface = selected
                .iter()
                .find(|(_, window)| *window == entry.hwnd)
                .map(|(surface, _)| *surface);
            if entry
                .capture
                .as_ref()
                .is_some_and(|capture| Some(capture.surface_id()) != surface)
            {
                entry.capture = None;
            }
        }
        let Some((surface, _)) = selected.into_iter().find(|(_, window)| *window == hwnd) else {
            return;
        };
        let stored = self.entries.get(&key).expect("presenting window inserted");
        let mut entry = stored.lock();
        let capture = entry
            .capture
            .get_or_insert_with(|| Capture::new(surface, hwnd, width, height));
        capture.set_requirements(width, height, hwnd);
        unsafe {
            capture.tick();
            capture.copy_frame(StreamId((context as u64) ^ surface.0.rotate_left(17)));
        }
    }
    pub(super) fn context_destroyed(&self, context: usize) {
        self.entries.retain(|&(_, owner), stored| {
            if owner != context {
                return true;
            }
            if let Some(capture) = stored.get_mut().capture.take() {
                capture.context_destroyed();
            }
            false
        });
    }
    pub(super) unsafe fn retire_context(&self, context: usize) {
        self.entries.retain(|&(_, owner), _| owner != context);
    }
    pub(super) unsafe fn retire_idle(&self, context: usize, now: u64) {
        self.entries.retain(|&(_, owner), stored| {
            owner != context || now.saturating_sub(stored.get_mut().last_swap) <= 600
        });
    }
    fn ordered_windows(&self, now: u64) -> Vec<isize> {
        let mut observed = self
            .entries
            .iter()
            .filter_map(|stored| {
                let entry = stored.lock();
                (now.saturating_sub(entry.last_swap) <= 240)
                    .then_some((entry.first_swap, entry.hwnd))
            })
            .collect::<Vec<_>>();
        observed.sort_unstable();
        let mut windows = Vec::new();
        for (_, window) in observed {
            if !windows.contains(&window) {
                windows.push(window);
            }
        }
        windows
    }

    pub(super) fn selected_windows(&self, now: u64) -> Vec<(SurfaceId, isize)> {
        let process = polymer_platform_windows::ProcessId(std::process::id());
        let windows = self
            .ordered_windows(now)
            .into_iter()
            .filter(|window| {
                polymer_platform_windows::is_process_window(NativeWindow(*window), process)
            })
            .collect::<Vec<_>>();
        let configuration =
            config::render_configuration().expect("validated capture configuration");
        if configuration.surfaces.is_empty() {
            return windows
                .into_iter()
                .map(|window| (SurfaceId(window as u64), window))
                .collect();
        }
        let patterns = self.patterns.get_or_init(|| {
            configuration
                .surfaces
                .iter()
                .map(|surface| {
                    surface
                        .pattern
                        .as_ref()
                        .map(|pattern| regex::Regex::new(pattern).expect("validated pattern"))
                })
                .collect()
        });
        let requested = configuration
            .surfaces
            .iter()
            .map(|surface| surface.index as usize)
            .collect::<Vec<_>>();
        let selection = windows
            .iter()
            .map(|&window| (window, window))
            .collect::<Vec<_>>();
        PresentationRegistry::resolve(&requested, &selection, |slot, window| {
            patterns[slot].as_ref().map(|pattern| {
                window_title(NativeWindow(window)).is_some_and(|title| pattern.is_match(&title))
            })
        })
        .into_iter()
        .map(|(slot, window)| {
            (
                SurfaceId(PresentationRegistry::logical_surface_id(
                    configuration.surfaces[slot].index as usize,
                )),
                window,
            )
        })
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_selection_deduplicates_contexts_and_expires_idle_drawables() {
        let store = CaptureStore::default();
        for (key, window, first, last) in [
            ((1, 10), 100, 1, 500),
            ((2, 20), 100, 2, 500),
            ((3, 10), 200, 3, 1),
            ((4, 30), 300, 4, 500),
        ] {
            store.entries.insert(
                key,
                Mutex::new(WindowCaptures {
                    hwnd: window,
                    first_swap: first,
                    last_swap: last,
                    capture: None,
                }),
            );
        }
        assert_eq!(store.ordered_windows(501), [100, 300]);
        // Only metadata is populated; no native GL objects are used by this test.
        unsafe { store.retire_idle(20, 1101) };
        assert!(!store.entries.contains_key(&(2, 20)));
        assert!(store.entries.contains_key(&(1, 10)));
        unsafe { store.retire_context(10) };
        assert_eq!(store.entries.len(), 1);
        assert!(store.entries.contains_key(&(4, 30)));
    }
}
