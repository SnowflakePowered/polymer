use parking_lot::{Mutex, RwLock};
use polymer_controllers::mapping::HostSample;
use polymer_ipc::input::InputMessage;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

#[derive(Default)]
pub struct HostState {
    sample: RwLock<HostSample>,
    events: Mutex<VecDeque<InputMessage>>,
    moved_at: Mutex<Option<Instant>>,
}

impl HostState {
    pub fn sample(&self) -> HostSample {
        let mut sample = self.sample.read().clone();
        let weight = self.moved_at.lock().map_or(0.0, |moved_at| {
            let elapsed = moved_at.elapsed();
            if elapsed <= Duration::from_millis(16) {
                1.0
            } else if elapsed >= Duration::from_millis(50) {
                0.0
            } else {
                1.0 - (elapsed.as_secs_f32() - 0.016) / 0.034
            }
        });
        sample.delta[0] *= weight;
        sample.delta[1] *= weight;
        sample
    }

    pub fn drain(&self) -> Vec<InputMessage> {
        self.events.lock().drain(..).collect()
    }

    pub fn receive(&self, event: InputMessage) {
        if matches!(
            event,
            InputMessage::HostKey(_) | InputMessage::HostPointer(_) | InputMessage::HostWheel(_)
        ) {
            let mut events = self.events.lock();
            if events.len() == 1024 {
                events.pop_front();
            }
            events.push_back(event);
        }
        let mut sample = self.sample.write();
        match event {
            InputMessage::HostKey(value) => sample.keys.set(
                polymer_input::KeyCode::from_wire(value.code).as_str(),
                value.pressed,
            ),
            InputMessage::HostPointer(value) => {
                if value.coordinate_width != 0 && value.coordinate_height != 0 {
                    sample.pointer = Some(value.position.map(Self::axis));
                    sample.mouse_buttons = value.buttons as u8;
                } else {
                    // Raw motion and surface position arrive separately. A
                    // position update has no delta and must not erase motion
                    // before the emulator polls it.
                    sample.delta = value.delta;
                    *self.moved_at.lock() = Some(Instant::now());
                }
            }
            _ => {}
        }
    }

    fn axis(value: f32) -> i16 {
        (value.clamp(0.0, 1.0) * 65535.0 - 32768.0)
            .round()
            .clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16
    }
}
