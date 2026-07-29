//! Shell policy for physical input capture.
//!
//! The emulator instance owns IPC. This adapter only gives long-lived shell
//! input objects a stable endpoint while game instances are replaced.

use parking_lot::RwLock;
use snowglass_input::provider::sdl3::{ProviderConfig, Sdl3Provider};
use snowglass_ipc::{Message, MessageSink};
use std::sync::Arc;

#[derive(Clone, Default)]
pub struct RoutedInputSender(Arc<RwLock<Option<snowglass_runtime::ProtocolInputSender>>>);

impl RoutedInputSender {
    fn attach(&self, instance: &snowglass_runtime::EmulatorInstance) {
        *self.0.write() = Some(instance.protocol_input_sender());
    }

    fn detach(&self) {
        *self.0.write() = None;
    }
}

impl MessageSink for RoutedInputSender {
    fn send_message(&self, message: Message) -> bool {
        self.0
            .read()
            .as_ref()
            .is_some_and(|sender| sender.send(message))
    }
}

pub struct InputTransport {
    control: RoutedInputSender,
}

impl InputTransport {
    pub fn runtime() -> Self {
        let control = RoutedInputSender::default();

        Self { control }
    }

    pub fn attach(&self, instance: &snowglass_runtime::EmulatorInstance) {
        self.control.attach(instance);
    }

    pub fn detach(&self) {
        self.control.detach();
    }

    pub fn start_provider(
        &self,
        config: ProviderConfig,
        instance: &snowglass_runtime::EmulatorInstance,
    ) -> Result<Sdl3Provider, String> {
        Sdl3Provider::start(config, instance.gamepad_input())
    }

    pub fn control_sender(&self) -> RoutedInputSender {
        self.control.clone()
    }
}
