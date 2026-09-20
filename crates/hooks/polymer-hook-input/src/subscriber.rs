use crate::HookInput;
use parking_lot::Mutex;
use polymer_controllers::topology::InputTopology;
use polymer_input::Feedback;
use polymer_ipc::{BlobKind, Message, WireFeedback, input::InputSubscriber};
use std::{
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

#[derive(Clone)]
pub struct ControlSender(Arc<Mutex<mpsc::Sender<(u8, Feedback)>>>);

impl ControlSender {
    pub fn new() -> (Self, mpsc::Receiver<(u8, Feedback)>) {
        let (sender, receiver) = mpsc::channel();
        (Self(Arc::new(Mutex::new(sender))), receiver)
    }

    pub fn feedback(&self, player: u8, feedback: Feedback) -> bool {
        self.0.lock().send((player, feedback)).is_ok()
    }
}

pub struct Subscriber;

impl Subscriber {
    pub fn spawn(name: String, input: Arc<HookInput>, feedback: mpsc::Receiver<(u8, Feedback)>) {
        let _ = thread::Builder::new()
            .name("polymer-hook-input".to_owned())
            .spawn(move || {
                loop {
                    match InputSubscriber::connect(&name) {
                        Ok(mut subscriber) => {
                            input.set_connected(true);
                            Self::run(&mut subscriber, &input, &feedback);
                            input.set_connected(false);
                        }
                        Err(_) => thread::sleep(Duration::from_millis(100)),
                    }
                }
            });
    }

    fn run(
        subscriber: &mut InputSubscriber,
        input: &HookInput,
        feedback: &mpsc::Receiver<(u8, Feedback)>,
    ) {
        loop {
            if subscriber.drain(|event| input.receive(event)).is_err() {
                return;
            }
            while let Ok((player, value)) = feedback.try_recv() {
                let Feedback::Rumble {
                    low_frequency,
                    high_frequency,
                    duration_ms,
                } = value
                else {
                    continue;
                };
                if subscriber
                    .control
                    .send(Message::GamepadFeedback(WireFeedback {
                        player,
                        low_frequency,
                        high_frequency,
                        duration_ms,
                    }))
                    .is_err()
                {
                    return;
                }
            }
            let _ = subscriber
                .control
                .set_read_timeout(Some(Duration::from_millis(1)));
            match subscriber.control.receive() {
                Ok(Message::Blob {
                    kind: BlobKind::Topology,
                    data: bytes,
                }) => {
                    if let Ok((value, _)) = bincode::serde::decode_from_slice::<InputTopology, _>(
                        &bytes,
                        bincode::config::standard(),
                    ) {
                        input.set_topology(value);
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) => {}
                Err(_) => return,
                Ok(message) => input.receive_control(message),
            }
            thread::yield_now();
        }
    }
}
