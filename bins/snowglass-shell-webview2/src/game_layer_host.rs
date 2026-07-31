//! Owns the background game layer lifecycle and its foreground bridge.
//!
//! `ShellState` decides what shell-wide effects a background request has, while
//! this type manages the proxy transport and layer lifecycle.
//! Emulator commands bypass it and enter the shell through the event loop.

use crate::game_layer::{GameLayer, GameLayerFactory};
use serde_json::Value;
use snowglass_protocol::AdapterLuid;
use std::error::Error;
use wry::WebView;

pub(crate) struct GameLayerHost {
    slot: GameLayerSlot,
}

enum GameLayerSlot {
    Awaiting(GameLayerFactory),
    Active(GameLayer),
    Disabled,
}

impl GameLayerHost {
    pub fn new(factory: GameLayerFactory) -> Self {
        Self {
            slot: GameLayerSlot::Awaiting(factory),
        }
    }

    pub fn active(&self) -> Option<&GameLayer> {
        match &self.slot {
            GameLayerSlot::Active(layer) => Some(layer),
            GameLayerSlot::Awaiting(_) | GameLayerSlot::Disabled => None,
        }
    }

    pub fn active_mut(&mut self) -> Option<&mut GameLayer> {
        match &mut self.slot {
            GameLayerSlot::Active(layer) => Some(layer),
            GameLayerSlot::Awaiting(_) | GameLayerSlot::Disabled => None,
        }
    }

    pub fn activate(&mut self, create: impl FnOnce(GameLayerFactory) -> GameLayer) -> bool {
        let slot = std::mem::replace(&mut self.slot, GameLayerSlot::Disabled);
        match slot {
            GameLayerSlot::Awaiting(factory) => {
                self.slot = GameLayerSlot::Active(create(factory));
                true
            }
            slot => {
                self.slot = slot;
                false
            }
        }
    }

    pub fn disable(&mut self) {
        if matches!(self.slot, GameLayerSlot::Awaiting(_)) {
            self.slot = GameLayerSlot::Disabled;
        }
    }

    pub fn switch_adapter(&mut self, adapter: AdapterLuid) -> Result<bool, Box<dyn Error>> {
        match &mut self.slot {
            GameLayerSlot::Active(layer) => layer.switch_adapter(adapter),
            GameLayerSlot::Awaiting(factory) => factory.switch_adapter(adapter),
            GameLayerSlot::Disabled => Ok(false),
        }
    }

    pub fn post_message(&mut self, message: &Value) -> bool {
        let Some(layer) = self.active_mut() else {
            return false;
        };
        layer.post_message(message);
        true
    }

    pub fn proxy_request(&mut self, request: &Value, foreground: &WebView) -> bool {
        let result = self
            .active_mut()
            .ok_or_else(|| "the foreground has no linked background document".to_owned())
            .and_then(|layer| layer.proxy_request(request));
        if let Err(error) = result {
            let id = request
                .get("id")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            Self::send_proxy_message(
                foreground,
                &serde_json::json!({
                    "type": "response",
                    "id": id,
                    "ok": false,
                    "error": {
                        "name": "Error",
                        "message": error,
                    },
                }),
            );
            return false;
        }
        true
    }

    pub fn poll_runtime(&mut self) -> bool {
        self.active_mut().is_some_and(GameLayer::poll_runtime)
    }

    pub fn set_input_configuration(&mut self, configuration: &Value, layouts: &Value) {
        if let Some(layer) = self.active_mut() {
            layer.set_input_configuration(configuration, layouts);
        }
    }

    pub fn input_configuration_result(&mut self, request_id: u64, result: &Value) {
        if let Some(layer) = self.active_mut() {
            layer.input_configuration_result(request_id, result);
        }
    }

    pub fn set_input_state(&mut self, controllers: &Value, priority: &str) {
        if let Some(layer) = self.active_mut() {
            layer.set_input_state(controllers, priority);
        }
    }

    pub fn flush(&mut self, foreground: &WebView) {
        let Some(layer) = self.active_mut() else {
            return;
        };
        for message in layer.take_messages() {
            let value = serde_json::to_string(&message).unwrap_or_else(|_| "null".to_owned());
            let script = format!("window.__snowglassGameLayerMessage({value});");
            if let Err(error) = foreground.evaluate_script(&script) {
                tracing::error!("snowglass: delivering a game-layer message failed: {error}");
            }
        }
        for message in layer.take_proxy_messages() {
            Self::send_proxy_message(foreground, &message);
        }
    }

    fn send_proxy_message(foreground: &WebView, message: &Value) {
        let value = serde_json::to_string(message).unwrap_or_else(|_| "null".to_owned());
        let script = format!("window.__snowglassGameLayerProxyMessage?.({value});");
        if let Err(error) = foreground.evaluate_script(&script) {
            tracing::error!("snowglass: delivering a game-layer proxy message failed: {error}");
        }
    }
}
