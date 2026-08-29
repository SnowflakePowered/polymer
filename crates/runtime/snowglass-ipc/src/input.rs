//! Shared-memory virtual input transport.

use crate::{
    control::{ChannelKind, LocalSocket, MapOffer, Message},
    shared,
};
use snowglass_input::gamepad::{GamepadUpdate, VirtualGamepadConnection};
use snowglass_protocol::{ConsumerApi, Handshake, PROTOCOL_VERSION};
use std::io;

pub struct InputPublisher {
    shared: shared::Publisher,
    generation: u64,
    latest: Vec<Option<GamepadUpdate>>,
    connections: Vec<Option<VirtualGamepadConnection>>,
}

impl InputPublisher {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            shared: shared::Publisher::create("input")?,
            generation: 1,
            latest: vec![None; u8::MAX as usize + 1],
            connections: vec![None; u8::MAX as usize + 1],
        })
    }

    pub fn accept(&mut self, control: &mut LocalSocket) -> io::Result<u32> {
        let Message::Hello(hello) = control.receive()? else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "expected input hello",
            ));
        };
        let handle = self.shared.duplicate_for(hello.process_id)?;
        control.send(Message::MapOffer(MapOffer {
            channel: ChannelKind::Input,
            handle,
            mapping_len: self.shared.mapping_len(),
            generation: self.generation,
        }))?;
        if control.receive()? != Message::MapAccepted {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "input map was not accepted",
            ));
        }
        for update in self.latest.iter().flatten() {
            self.shared
                .publish(&crate::control::encode_input(Message::GamepadUpdate(
                    *update,
                )))?;
        }
        for connection in self.connections.iter().flatten() {
            self.shared.publish(&crate::control::encode_input(
                Message::VirtualGamepadConnection(*connection),
            ))?;
        }
        Ok(hello.process_id)
    }

    pub fn publish(&mut self, update: GamepadUpdate) -> io::Result<()> {
        self.latest[update.slot.index()] = Some(update);
        self.shared
            .publish(&crate::control::encode_input(Message::GamepadUpdate(
                update,
            )))
    }

    pub fn set_connected(&mut self, connection: VirtualGamepadConnection) -> io::Result<()> {
        self.connections[connection.slot.index()] = Some(connection);
        self.shared.publish(&crate::control::encode_input(
            Message::VirtualGamepadConnection(connection),
        ))
    }
}

pub struct InputSubscriber {
    pub control: LocalSocket,
    shared: shared::Subscriber,
    scratch: Vec<u8>,
}

impl InputSubscriber {
    pub fn connect(name: &str) -> io::Result<Self> {
        let mut control = LocalSocket::connect(name)?;
        control.send(Message::Hello(Handshake {
            protocol_version: PROTOCOL_VERSION,
            process_id: std::process::id(),
            preferred_consumer: ConsumerApi::Vulkan,
        }))?;
        let Message::MapOffer(offer) = control.receive()? else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "expected input map offer",
            ));
        };
        if offer.channel != ChannelKind::Input {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "wrong shared channel kind",
            ));
        }
        // SAFETY: the publisher duplicated this handle into this process and
        // transferred ownership in the map offer.
        let shared = unsafe { shared::Subscriber::from_handle(offer.handle, offer.mapping_len)? };
        control.send(Message::MapAccepted)?;
        Ok(Self {
            control,
            shared,
            scratch: Vec::new(),
        })
    }

    pub fn drain(&mut self, mut receive: impl FnMut(GamepadUpdate)) -> io::Result<usize> {
        self.drain_events(|event| {
            if let Message::GamepadUpdate(update) = event {
                receive(update);
            }
        })
    }

    pub fn drain_events(&mut self, mut receive: impl FnMut(Message)) -> io::Result<usize> {
        self.shared.drain(&mut self.scratch, |bytes| {
            receive(crate::control::decode_input(bytes)?);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocalSocketListener;
    use snowglass_input::gamepad::{GamepadSlot, GamepadState};

    #[test]
    fn negotiates_mapping_and_forwards_state() {
        let name = format!("snowglass-input-map-test-{}", std::process::id());
        let listener = LocalSocketListener::bind(&name).unwrap();
        let expected = GamepadUpdate {
            slot: GamepadSlot::new(7).unwrap(),
            sequence: 42,
            connected: true,
            state: GamepadState {
                buttons: 5,
                ..GamepadState::default()
            },
        };
        let server = std::thread::spawn(move || {
            let mut publisher = InputPublisher::new().unwrap();
            let mut control = listener.accept().unwrap();
            publisher.accept(&mut control).unwrap();
            publisher.publish(expected).unwrap();
        });
        let mut subscriber = InputSubscriber::connect(&name).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let mut received = None;
        while received.is_none() && std::time::Instant::now() < deadline {
            subscriber.drain(|update| received = Some(update)).unwrap();
            std::thread::yield_now();
        }
        server.join().unwrap();
        assert_eq!(received, Some(expected));
    }

    #[test]
    fn broadcasts_followup_state_to_multiple_subscribers() {
        const SUBSCRIBERS: usize = 3;
        let name = format!("snowglass-input-broadcast-test-{}", std::process::id());
        let listener = LocalSocketListener::bind(&name).unwrap();
        let active = GamepadUpdate {
            slot: GamepadSlot::new(0).unwrap(),
            sequence: 2,
            connected: true,
            state: GamepadState {
                buttons: 1,
                ..GamepadState::default()
            },
        };
        let server = std::thread::spawn(move || {
            let mut publisher = InputPublisher::new().unwrap();
            for _ in 0..SUBSCRIBERS {
                let mut control = listener.accept().unwrap();
                publisher.accept(&mut control).unwrap();
            }
            publisher.publish(active).unwrap();
        });
        let subscribers = (0..SUBSCRIBERS)
            .map(|_| {
                let name = name.clone();
                std::thread::spawn(move || {
                    let mut subscriber = InputSubscriber::connect(&name).unwrap();
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
                    let mut received = None;
                    while received != Some(active) && std::time::Instant::now() < deadline {
                        subscriber
                            .drain(|update| {
                                if update == active {
                                    received = Some(update);
                                }
                            })
                            .unwrap();
                        std::thread::yield_now();
                    }
                    received
                })
            })
            .collect::<Vec<_>>();
        server.join().unwrap();
        for subscriber in subscribers {
            assert_eq!(subscriber.join().unwrap(), Some(active));
        }
    }

    #[test]
    fn replays_virtual_connection_state_to_late_subscribers() {
        let name = format!("snowglass-input-connection-test-{}", std::process::id());
        let listener = LocalSocketListener::bind(&name).unwrap();
        let expected = VirtualGamepadConnection {
            slot: GamepadSlot::new(5).unwrap(),
            connected: false,
        };
        let server = std::thread::spawn(move || {
            let mut publisher = InputPublisher::new().unwrap();
            publisher.set_connected(expected).unwrap();
            let mut control = listener.accept().unwrap();
            publisher.accept(&mut control).unwrap();
        });
        let mut subscriber = InputSubscriber::connect(&name).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let mut received = None;
        while received.is_none() && std::time::Instant::now() < deadline {
            subscriber
                .drain_events(|event| {
                    if let Message::VirtualGamepadConnection(connection) = event {
                        received = Some(connection);
                    }
                })
                .unwrap();
            std::thread::yield_now();
        }
        server.join().unwrap();
        assert_eq!(received, Some(expected));
    }
}
