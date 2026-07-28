//! Owns the background game layer and its foreground bridge.
//!
//! `ShellState` decides what shell-wide effects a background request has, but
//! it does not manage the Deno queues, proxy protocol, or layer lifecycle.

use crate::{
    api::LaunchOptions,
    game_layer::{GameLayer, GameLayerCommand, GameLayerFactory},
};
use serde_json::Value;
use wry::WebView;

pub(crate) enum GameLayerAction {
    Launch {
        instance_id: String,
        options: LaunchOptions,
    },
    Reject {
        instance_id: String,
        message: String,
    },
    Close(String),
    Message {
        instance_id: String,
        name: String,
        args: Value,
    },
    Input(bool),
}

pub(crate) struct GameLayerWork {
    pub needs_refresh: bool,
    pub actions: Vec<GameLayerAction>,
}

pub(crate) struct GameLayerState {
    layer: Option<GameLayer>,
    factory: Option<GameLayerFactory>,
    chrome_bands_drawn: Vec<usize>,
    chrome_dirty: bool,
    refresh_pending: bool,
    mouse_buttons: u8,
}

impl GameLayerState {
    pub fn new(factory: GameLayerFactory) -> Self {
        Self {
            layer: None,
            factory: Some(factory),
            chrome_bands_drawn: Vec::new(),
            chrome_dirty: false,
            refresh_pending: false,
            mouse_buttons: 0,
        }
    }

    pub fn layer(&self) -> Option<&GameLayer> {
        self.layer.as_ref()
    }

    pub fn layer_mut(&mut self) -> Option<&mut GameLayer> {
        self.layer.as_mut()
    }

    pub fn is_linked(&self) -> bool {
        self.layer.is_some()
    }

    pub fn take_factory(&mut self) -> Option<GameLayerFactory> {
        self.factory.take()
    }

    pub fn discard_factory(&mut self) {
        self.factory = None;
    }

    pub fn install(&mut self, layer: GameLayer) {
        self.layer = Some(layer);
    }

    pub fn chrome_bands(&self) -> &[usize] {
        &self.chrome_bands_drawn
    }

    pub fn set_painted_chrome(&mut self, bands: usize, merged: bool) {
        self.chrome_bands_drawn = (0..bands).collect();
        self.chrome_dirty = merged;
    }

    pub fn chrome_dirty(&self) -> bool {
        self.chrome_dirty
    }

    pub fn clear_chrome_dirty(&mut self) {
        self.chrome_dirty = false;
    }

    pub fn defer_refresh(&mut self) {
        self.refresh_pending = true;
    }

    pub fn take_refresh_pending(&mut self) -> bool {
        std::mem::take(&mut self.refresh_pending)
    }

    pub fn mouse_buttons(&self) -> u8 {
        self.mouse_buttons
    }

    pub fn set_mouse_button(&mut self, flag: u8, pressed: bool) -> u8 {
        let previous = self.mouse_buttons;
        if pressed {
            self.mouse_buttons |= flag;
        } else {
            self.mouse_buttons &= !flag;
        }
        previous
    }

    pub fn restore_mouse_buttons(&mut self, buttons: u8) {
        self.mouse_buttons = buttons;
    }

    pub fn clear_mouse_buttons(&mut self) {
        self.mouse_buttons = 0;
    }

    pub fn post_message(&mut self, message: &Value) -> bool {
        let Some(layer) = self.layer.as_mut() else {
            return false;
        };
        layer.post_message(message);
        true
    }

    pub fn proxy_request(&mut self, request: &Value, foreground: &WebView) -> bool {
        let result = self
            .layer
            .as_mut()
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
        self.layer
            .as_mut()
            .is_some_and(GameLayer::poll_runtime)
    }

    pub fn flush(&mut self, foreground: &WebView) {
        let Some(layer) = self.layer.as_mut() else {
            return;
        };
        for message in layer.take_messages() {
            let value = serde_json::to_string(&message).unwrap_or_else(|_| "null".to_owned());
            let script = format!("window.__snowglassGameLayerMessage({value});");
            if let Err(error) = foreground.evaluate_script(&script) {
                eprintln!("snowglass: delivering a game-layer message failed: {error}");
            }
        }
        for message in layer.take_proxy_messages() {
            Self::send_proxy_message(foreground, &message);
        }
    }

    pub fn service(&mut self, foreground: &WebView) -> GameLayerWork {
        self.flush(foreground);
        let needs_refresh = self.layer.as_ref().is_some_and(GameLayer::needs_paint);
        let commands = self
            .layer
            .as_mut()
            .map(GameLayer::take_commands)
            .unwrap_or_default();
        let actions = commands.into_iter().map(Self::action).collect();
        GameLayerWork {
            needs_refresh,
            actions,
        }
    }

    fn action(command: GameLayerCommand) -> GameLayerAction {
        match command {
            GameLayerCommand::Launch {
                instance_id,
                options,
            } => match serde_json::from_value::<LaunchOptions>(options) {
                Ok(options) => match options.validate() {
                    Ok(()) => GameLayerAction::Launch {
                        instance_id,
                        options,
                    },
                    Err(message) => GameLayerAction::Reject {
                        instance_id,
                        message,
                    },
                },
                Err(error) => GameLayerAction::Reject {
                    instance_id,
                    message: format!("invalid LaunchOptions: {error}"),
                },
            },
            GameLayerCommand::Close(instance_id) => GameLayerAction::Close(instance_id),
            GameLayerCommand::Message {
                instance_id,
                name,
                args,
            } => GameLayerAction::Message {
                instance_id,
                name,
                args,
            },
            GameLayerCommand::Input(active) => GameLayerAction::Input(active),
        }
    }

    fn send_proxy_message(foreground: &WebView, message: &Value) {
        let value = serde_json::to_string(message).unwrap_or_else(|_| "null".to_owned());
        let script = format!("window.__snowglassGameLayerProxyMessage?.({value});");
        if let Err(error) = foreground.evaluate_script(&script) {
            eprintln!("snowglass: delivering a game-layer proxy message failed: {error}");
        }
    }
}
