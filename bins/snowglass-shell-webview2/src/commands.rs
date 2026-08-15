//! Commands crossing the shell's foreground, runtime, and event-loop boundaries.

use crate::{
    cli::GameLaunchRequest, game_host::GameHostCommand, input::InputCommand, launcher,
    window_manager::WindowCommand,
};

#[derive(Debug)]
pub(crate) enum UserEvent {
    Window(WindowCommand),
    Runtime(RuntimeCommand),
    GameHost(GameHostCommand),
    Input(InputCommand),
}

#[derive(Debug)]
pub(crate) enum RuntimeCommand {
    FrontendReady {
        background: Option<BackgroundDocumentSource>,
        background_error: Option<String>,
    },
    ForegroundNavigation,
    SharedBusDoorbell,
    SharedBusSettlement {
        generation: u32,
        request_id: u64,
        response_lane: usize,
        result: Result<Vec<u8>, snowglass_pluginhost::PluginInvocationError>,
    },
    LauncherOpen(GameLaunchRequest),
    LauncherStart(launcher::LauncherStartRequest),
    LauncherCancel,
    GameDetected {
        generation: u64,
        result: Result<launcher::DetectedGame, String>,
    },
}

#[derive(Debug)]
pub(crate) struct BackgroundDocumentSource {
    pub(crate) html: String,
    pub(crate) base_url: String,
}

pub(crate) fn decode_foreground_command(message: serde_json::Value) -> Result<UserEvent, String> {
    let kind = message
        .get("type")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "foreground shell command is missing its type".to_owned())?;
    match kind {
        "frontendReady" => {
            let background = message.get("background").and_then(|background| {
                Some(BackgroundDocumentSource {
                    html: background.get("html")?.as_str()?.to_owned(),
                    base_url: background.get("url")?.as_str()?.to_owned(),
                })
            });
            let background_error = message
                .get("backgroundError")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            Ok(UserEvent::Runtime(RuntimeCommand::FrontendReady {
                background,
                background_error,
            }))
        }
        "foregroundHitRegions" => {
            let regions = message
                .get("regions")
                .cloned()
                .ok_or_else(|| "foreground hit-region command is missing regions".to_owned())?;
            serde_json::from_value(regions)
                .map(|regions| UserEvent::Input(InputCommand::ForegroundHitRegions(regions)))
                .map_err(|error| format!("invalid WebView2 foreground hit regions: {error}"))
        }
        "inputConfigure" => {
            let request_id = message
                .get("id")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| "input configuration command has an invalid ID".to_owned())?;
            let transaction = message.get("transaction").cloned().ok_or_else(|| {
                "input configuration command is missing its transaction".to_owned()
            })?;
            serde_json::from_value(transaction)
                .map(|transaction| {
                    UserEvent::Input(InputCommand::Configure {
                        request_id,
                        transaction,
                    })
                })
                .map_err(|error| format!("invalid input configuration transaction: {error}"))
        }
        "launcherOpen" => {
            let game = message
                .get("path")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "launcher-open command is missing its path".to_owned())?
                .into();
            let emulator = message
                .get("emulator")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            Ok(UserEvent::Runtime(RuntimeCommand::LauncherOpen(
                GameLaunchRequest { game, emulator },
            )))
        }
        "launcherStart" => {
            let request = message
                .get("request")
                .cloned()
                .ok_or_else(|| "launcher-start command is missing its request".to_owned())?;
            serde_json::from_value(request)
                .map(|request| UserEvent::Runtime(RuntimeCommand::LauncherStart(request)))
                .map_err(|error| format!("invalid frontend launch request: {error}"))
        }
        "launcherCancel" => Ok(UserEvent::Runtime(RuntimeCommand::LauncherCancel)),
        other => Err(format!("unsupported foreground shell command '{other}'")),
    }
}

pub(crate) fn launcher_state(state: &crate::launcher::LauncherState) -> Option<serde_json::Value> {
    let state = serde_json::to_value(state).ok()?;
    Some(serde_json::json!({ "type": "launcherState", "state": state }))
}

pub(crate) fn cli_launch(
    options: &crate::launch::UnattachedLaunchPlan,
) -> Option<serde_json::Value> {
    let options = serde_json::to_value(options).ok()?;
    Some(serde_json::json!({
        "type": "snowglass-cli-launch",
        "options": options,
    }))
}

pub(crate) fn game_layer_ready() -> serde_json::Value {
    serde_json::json!({ "type": "gameLayerReady" })
}

#[cfg(test)]
mod tests {
    use super::{RuntimeCommand, UserEvent, decode_foreground_command};

    #[test]
    fn launcher_start_accepts_safe_integers_after_shared_bus_round_trip() {
        let command = serde_json::json!({
            "type": "launcherStart",
            "request": {
                "generation": 1,
                "emulator": "cemu",
                "groups": [{
                    "name": "controller",
                    "ports": [{ "index": 0, "layout": "WIIU_PRO_CONTROLLER", "device": "K" }]
                }],
                "settings": { "content-graphic.override_gamma_value": 2.2 }
            }
        });
        let command = snowglass_pluginhost::bus::StructuredValue::from_json(&command)
            .unwrap()
            .to_json()
            .unwrap();

        let event = decode_foreground_command(command).unwrap();
        let UserEvent::Runtime(RuntimeCommand::LauncherStart(request)) = event else {
            panic!("launcher command decoded to the wrong event");
        };
        assert_eq!(request.generation, 1);
        assert_eq!(request.groups[0].ports[0].index, 0);
        assert_eq!(
            request.settings["content-graphic.override_gamma_value"].as_f64(),
            Some(2.2)
        );
    }
}
