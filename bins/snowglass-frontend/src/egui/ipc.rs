use serde::Serialize;

/// Renderer diagnostics exposed to the optional canvas debug layer.
///
/// Keeping this DTO in the frontend prevents Tauri types and commands from
/// leaking into `snowglass-renderer`.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DebugSnapshot {
    backend: &'static str,
    consumer: &'static str,
    frame_number: u64,
    frames_per_second: f64,
    latency_ms: f64,
}

#[tauri::command]
pub(crate) fn debug_snapshot() -> DebugSnapshot {
    DebugSnapshot {
        backend: "waiting",
        consumer: "waiting",
        ..DebugSnapshot::default()
    }
}
