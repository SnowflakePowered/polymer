use parking_lot::{Mutex, RwLock};
use snowglass_input_layout::mapping::HostSample;
use snowglass_ipc::input::InputMessage;
use std::collections::VecDeque;

#[derive(Default)]
pub struct HostState {
    sample: RwLock<HostSample>,
    events: Mutex<VecDeque<InputMessage>>,
}

impl HostState {
    pub fn sample(&self) -> HostSample {
        self.sample.read().clone()
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
                snowglass_input::KeyCode::from_wire(value.code).as_str(),
                value.pressed,
            ),
            InputMessage::HostPointer(value) => {
                sample.pointer = Some(value.position.map(Self::axis));
                sample.delta = value.delta;
                sample.mouse_buttons = value.buttons as u8;
            }
            _ => {}
        }
    }

    fn axis(value: f32) -> i16 {
        (value.clamp(0.0, 1.0) * f32::from(i16::MAX)) as i16
    }
}
