//! Outbound emulator-instance event payloads consumed by the background UI.

use crate::{background_runtime::InstanceToken, game_host::GameSurfaceTarget};

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct WebRenderTarget {
    connection: String,
    index: u32,
    width: u32,
    height: u32,
}

pub(crate) fn envelope(
    token: InstanceToken,
    event_name: &str,
    payload: &serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "instanceId": token,
        "eventName": event_name,
        "payload": payload,
    })
}

pub(crate) fn message(name: &str, args: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "name": name, "args": args })
}

pub(crate) fn error(message: impl ToString) -> serde_json::Value {
    serde_json::json!({ "message": message.to_string() })
}

pub(crate) fn audio(audio: snowglass_runtime::AudioState) -> serde_json::Value {
    serde_json::json!({
        "available": audio.available,
        "volume": audio.volume.get(),
        "muted": audio.muted,
    })
}

pub(crate) fn surfaces(surfaces: Vec<GameSurfaceTarget>) -> serde_json::Value {
    let mut targets = surfaces
        .into_iter()
        .map(|surface| WebRenderTarget {
            connection: surface.connection.to_string(),
            index: surface.index,
            width: surface.width,
            height: surface.height,
        })
        .collect::<Vec<_>>();
    targets.sort_by_key(|target| target.index);
    serde_json::to_value(targets).expect("render target serialization is infallible")
}
