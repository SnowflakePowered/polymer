//! Owns the background game layer and its foreground bridge.
//!
//! `ShellState` decides what shell-wide effects a background request has, while
//! this type manages the proxy transport and layer presentation lifecycle.
//! Emulator commands bypass it and enter the shell through the event loop.

use crate::game_layer::{GameLayer, GameLayerFactory};
use serde_json::Value;
use snowglass_protocol::AdapterLuid;
use std::error::Error;
use wry::WebView;

pub(crate) struct GameLayerState {
    layer: Option<GameLayer>,
    factory: Option<GameLayerFactory>,
    presentation: GameLayerPresentationState,
}

struct GameLayerPresentationState {
    chrome_bands_drawn: Vec<usize>,
    chrome_dirty: bool,
    refresh_pending: bool,
}

impl GameLayerState {
    pub fn new(factory: GameLayerFactory) -> Self {
        Self {
            layer: None,
            factory: Some(factory),
            presentation: GameLayerPresentationState {
                chrome_bands_drawn: Vec::new(),
                chrome_dirty: false,
                refresh_pending: false,
            },
        }
    }

    pub fn layer(&self) -> Option<&GameLayer> {
        self.layer.as_ref()
    }

    pub fn layer_mut(&mut self) -> Option<&mut GameLayer> {
        self.layer.as_mut()
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

    pub fn switch_adapter(&mut self, adapter: AdapterLuid) -> Result<bool, Box<dyn Error>> {
        match (&mut self.layer, &mut self.factory) {
            (Some(layer), _) => layer.switch_adapter(adapter),
            (None, Some(factory)) => factory.switch_adapter(adapter),
            (None, None) => Ok(false),
        }
    }

    pub fn chrome_bands(&self) -> &[usize] {
        &self.presentation.chrome_bands_drawn
    }

    pub fn set_painted_chrome(&mut self, bands: usize) {
        self.presentation.chrome_bands_drawn = (0..bands).collect();
        self.presentation.chrome_dirty = true;
    }

    pub fn chrome_dirty(&self) -> bool {
        self.presentation.chrome_dirty
    }

    pub fn clear_chrome_dirty(&mut self) {
        self.presentation.chrome_dirty = false;
    }

    pub fn defer_refresh(&mut self) {
        self.presentation.refresh_pending = true;
    }

    pub fn take_refresh_pending(&mut self) -> bool {
        std::mem::take(&mut self.presentation.refresh_pending)
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
        self.layer.as_mut().is_some_and(GameLayer::poll_runtime)
    }

    pub fn flush(&mut self, foreground: &WebView) {
        let Some(layer) = self.layer.as_mut() else {
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

    pub fn service(&mut self, foreground: &WebView) -> bool {
        self.flush(foreground);
        self.layer.as_ref().is_some_and(GameLayer::needs_paint)
    }

    fn send_proxy_message(foreground: &WebView, message: &Value) {
        let value = serde_json::to_string(message).unwrap_or_else(|_| "null".to_owned());
        let script = format!("window.__snowglassGameLayerProxyMessage?.({value});");
        if let Err(error) = foreground.evaluate_script(&script) {
            tracing::error!("snowglass: delivering a game-layer proxy message failed: {error}");
        }
    }
}
