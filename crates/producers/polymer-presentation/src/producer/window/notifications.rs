use crate::registry::PresentationRegistry;
use polymer_platform_windows::InlineDetour;
use std::{
    collections::VecDeque,
    sync::{Mutex, OnceLock, atomic::Ordering},
};

type Destroy = unsafe extern "system" fn(isize) -> i32;
#[link(name = "user32")]
unsafe extern "system" {
    #[link_name = "DestroyWindow"]
    fn destroy_window(window: isize) -> i32;
}
static HOOK: InlineDetour<Destroy> = InlineDetour::new(destroy_window, DestructionEvents::destroy);
static INSTALL: OnceLock<Result<(), i32>> = OnceLock::new();
static EVENTS: Mutex<DestructionEvents> = Mutex::new(DestructionEvents {
    generation: 0,
    windows: VecDeque::new(),
});

pub(super) struct DestructionEvents {
    generation: u64,
    windows: VecDeque<(u64, isize)>,
}
impl DestructionEvents {
    unsafe extern "system" fn destroy(window: isize) -> i32 {
        let result = unsafe { HOOK.trampoline()(window) };
        if result != 0 {
            let mut events = EVENTS.lock().unwrap_or_else(|error| error.into_inner());
            events.generation += 1;
            let generation = events.generation;
            events.windows.push_back((generation, window));
            if events.windows.len() > 256 {
                events.windows.pop_front();
            }
        }
        result
    }

    pub(super) fn refresh(registry: &PresentationRegistry) {
        if let Err(error) = INSTALL.get_or_init(|| HOOK.enable()) {
            tracing::debug!(error, "window destruction tracking unavailable");
            return;
        }
        let events = EVENTS.lock().unwrap_or_else(|error| error.into_inner());
        let previous = registry
            .window_event_generation
            .swap(events.generation, Ordering::Relaxed);
        let expired = events
            .windows
            .front()
            .is_some_and(|(generation, _)| previous + 1 < *generation);
        let destroyed = if expired {
            registry.observed_windows()
        } else {
            events
                .windows
                .iter()
                .filter_map(|(generation, window)| (*generation > previous).then_some(*window))
                .collect()
        };
        drop(events);
        for window in destroyed {
            registry.forget_window(window);
        }
    }
}
