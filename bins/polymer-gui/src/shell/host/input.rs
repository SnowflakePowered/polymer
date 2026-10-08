use super::ShellHost;
use crate::shell::{
    document::{DomKeyboardEvent, DomMouseEvent, DomWheelEvent, ShellHitSnapshot},
    protocol::{InputOutcome, PointerSample, ShellInput},
    runtime::CoalescedPointer,
};
use std::time::Instant;

impl ShellHost {
    /// Hit tests and dispatches one input against the current document.
    /// `coalesced` holds earlier pointer moves this input replaced.
    pub(super) fn dispatch(
        &mut self,
        input: ShellInput,
        coalesced: Vec<PointerSample>,
    ) -> InputOutcome {
        match input {
            ShellInput::Pointer {
                kind,
                sample,
                button,
                buttons,
                ctrl_key,
                shift_key,
            } => {
                let Some((hit, x, y)) = self.hit(sample.x as f32, sample.y as f32) else {
                    return InputOutcome::default();
                };
                let samples = self.coalesced_pointers(coalesced, sample);
                let claimed = self.document.mouse_event(
                    &mut self.runtime,
                    hit,
                    DomMouseEvent {
                        kind,
                        x,
                        y,
                        button,
                        buttons,
                        ctrl_key,
                        shift_key,
                    },
                    &samples,
                );
                InputOutcome {
                    claimed,
                    over_tracked_surface: hit.over_tracked_surface,
                }
            }
            ShellInput::Wheel {
                x,
                y,
                horizontal,
                vertical,
                ctrl_key,
                shift_key,
                alt_key,
            } => {
                let Some((hit, x, y)) = self.hit(x, y) else {
                    return InputOutcome::default();
                };
                let scale_factor = f64::from(self.document.0.borrow().scale_factor());
                let claimed = self.document.wheel_event(
                    &mut self.runtime,
                    hit,
                    DomWheelEvent {
                        x,
                        y,
                        horizontal: f64::from(horizontal) / scale_factor,
                        vertical: f64::from(vertical) / scale_factor,
                        ctrl_key,
                        shift_key,
                        alt_key,
                    },
                );
                InputOutcome {
                    claimed,
                    over_tracked_surface: hit.over_tracked_surface,
                }
            }
            ShellInput::Key {
                pressed,
                key,
                code,
                repeat,
                alt_key,
                ctrl_key,
                shift_key,
            } => InputOutcome {
                claimed: self.document.keyboard_event(
                    &mut self.runtime,
                    DomKeyboardEvent {
                        pressed,
                        key,
                        code,
                        repeat,
                        alt_key,
                        ctrl_key,
                        shift_key,
                    },
                ),
                over_tracked_surface: false,
            },
            ShellInput::Ime(event) => {
                self.document.ime_event(&mut self.runtime, event);
                InputOutcome::default()
            }
            ShellInput::PointerLeft => {
                self.document.0.borrow_mut().clear_pointer_hover();
                InputOutcome::default()
            }
        }
    }

    /// Converts a move and the moves it coalesced into CSS pixels, aged
    /// relative to now.
    fn coalesced_pointers(
        &self,
        coalesced: Vec<PointerSample>,
        sample: PointerSample,
    ) -> Vec<CoalescedPointer> {
        let document = self.document.0.borrow();
        let now = Instant::now();
        coalesced
            .into_iter()
            .chain(std::iter::once(sample))
            .map(|sample| {
                let (x, y) = document.physical_to_css(sample.x as f32, sample.y as f32);
                CoalescedPointer {
                    x,
                    y,
                    age_ms: now.saturating_duration_since(sample.at).as_secs_f64() * 1000.0,
                }
            })
            .collect()
    }

    /// Hit tests a physical-pixel position, returning the hit and the
    /// position in CSS pixels.
    fn hit(&self, x: f32, y: f32) -> Option<(ShellHitSnapshot, f32, f32)> {
        let document = self.document.0.borrow();
        let (x, y) = document.physical_to_css(x, y);
        document.hit_test(x, y).map(|hit| (hit, x, y))
    }
}
