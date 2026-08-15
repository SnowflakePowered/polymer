use crate::cli::GameLaunchRequest;
use crate::launch::{EmulatorLaunchArguments, UnattachedLaunchPlan, plugin_launch_options};
use crate::launcher;
use snowglass_renderer::cli::ViewerOptions;
use std::{collections::HashMap, sync::Arc, time::Instant};

pub(crate) enum LaunchCoordinatorEvent {
    GameDetected {
        generation: u64,
        result: Result<launcher::DetectedGame, String>,
    },
}

enum PendingLaunch {
    Ready(UnattachedLaunchPlan),
    Plugin(launcher::ConfiguredGameLaunch),
}

/// Owns launcher state, game detection, and plugin-backed launch preparation
/// for the long-lived background runtime. The shell only publishes immutable
/// state snapshots and forwards the resulting native launch plan.
pub(crate) struct BackgroundLaunchCoordinator {
    pending: Option<PendingLaunch>,
    plugins: snowglass_pluginhost::PluginRegistry,
    plugin_runtime: Arc<snowglass_pluginhost::PluginRuntime>,
    viewer: ViewerOptions,
    state: launcher::LauncherState,
    prepared_game: Option<launcher::PreparedGame>,
    detection_generation: u64,
    event_sink: Arc<dyn Fn(LaunchCoordinatorEvent) + Send + Sync>,
}

impl BackgroundLaunchCoordinator {
    pub fn new(
        pending: Option<UnattachedLaunchPlan>,
        plugins: snowglass_pluginhost::PluginRegistry,
        plugin_runtime: Arc<snowglass_pluginhost::PluginRuntime>,
        viewer: ViewerOptions,
        event_sink: impl Fn(LaunchCoordinatorEvent) + Send + Sync + 'static,
    ) -> Self {
        Self {
            pending: pending.map(PendingLaunch::Ready),
            plugins,
            plugin_runtime,
            viewer,
            state: launcher::LauncherState::Idle,
            prepared_game: None,
            detection_generation: 0,
            event_sink: Arc::new(event_sink),
        }
    }

    pub fn state(&self) -> &launcher::LauncherState {
        &self.state
    }

    pub fn plugin_runtime_handle(&self) -> snowglass_pluginhost::PluginRuntimeHandle {
        self.plugin_runtime.handle()
    }

    pub fn has_pending_launch(&self) -> bool {
        self.pending.is_some()
    }

    pub fn queue_game(&mut self, request: GameLaunchRequest) {
        self.detection_generation = self.detection_generation.wrapping_add(1);
        let generation = self.detection_generation;
        let path = request.game;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        self.prepared_game = None;
        self.state = launcher::LauncherState::Detecting {
            path: path.to_string_lossy().into_owned(),
            name,
        };
        let sink = Arc::clone(&self.event_sink);
        tokio::task::spawn_local(async move {
            let started = Instant::now();
            let result = launcher::detect_game(path, request.emulator).await;
            tracing::info!(
                elapsed_ms = started.elapsed().as_millis(),
                "snowglass: game detection task completed"
            );
            sink(LaunchCoordinatorEvent::GameDetected { generation, result });
        });
    }

    pub fn game_detected(
        &mut self,
        generation: u64,
        result: Result<launcher::DetectedGame, String>,
    ) {
        if generation != self.detection_generation {
            return;
        }
        let result = result.and_then(|detected| {
            tracing::info!(
                "snowglass: detected {} as {}",
                detected.path.display(),
                detected.platform
            );
            launcher::PreparedGame::new(
                generation,
                detected,
                self.plugins
                    .plugins::<dyn snowglass_pluginhost::EmulatorPluginInstance>(),
                self.viewer.clone(),
            )
        });
        match result {
            Ok(prepared) => {
                tracing::info!(
                    "snowglass: compatible emulator plugins: {}",
                    prepared
                        .descriptor()
                        .candidates
                        .iter()
                        .map(|candidate| candidate.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                self.state = launcher::LauncherState::Configure {
                    game: prepared.descriptor().clone(),
                };
                self.prepared_game = Some(prepared);
            }
            Err(error) => {
                tracing::error!("snowglass: game detection failed: {error}");
                self.state = launcher::LauncherState::Error { message: error };
                self.prepared_game = None;
            }
        }
    }

    pub fn start_prepared_game(
        &mut self,
        request: launcher::LauncherStartRequest,
        active_game: bool,
    ) {
        if active_game {
            self.set_error("Close the active game before launching another one.");
            return;
        }
        let Some(prepared) = self.prepared_game.as_ref() else {
            self.set_error("Choose a game before pressing Start.");
            return;
        };
        match prepared.start(request) {
            Ok(launch) => {
                let name = prepared.descriptor().name.clone();
                self.pending = Some(PendingLaunch::Plugin(launch));
                self.state = launcher::LauncherState::Launching { name };
                self.prepared_game = None;
            }
            Err(message) => self.state = launcher::LauncherState::Error { message },
        }
    }

    pub fn cancel(&mut self) {
        self.detection_generation = self.detection_generation.wrapping_add(1);
        self.prepared_game = None;
        self.state = launcher::LauncherState::Idle;
    }

    pub fn set_error(&mut self, message: impl Into<String>) {
        self.state = launcher::LauncherState::Error {
            message: message.into(),
        };
    }

    pub async fn resolve_pending_launch(&mut self) -> Option<Result<UnattachedLaunchPlan, String>> {
        let pending = self.pending.take()?;
        Some(match pending {
            PendingLaunch::Ready(plan) => Ok(plan),
            PendingLaunch::Plugin(request) => self.realize_plugin_launch(request).await,
        })
    }

    async fn realize_plugin_launch(
        &self,
        request: launcher::ConfiguredGameLaunch,
    ) -> Result<UnattachedLaunchPlan, String> {
        let port_assignments = request
            .configuration
            .port_assignments()
            .iter()
            .map(|(group, layouts)| {
                (
                    group.to_string(),
                    layouts.iter().map(ToString::to_string).collect::<Vec<_>>(),
                )
            })
            .collect::<HashMap<_, _>>();
        let arguments = serde_json::json!({
            "launchPath": request.game.to_string_lossy(),
            "console": request.viewer.machine,
            "portAssignments": port_assignments,
        });
        let plan: EmulatorLaunchArguments = self
            .plugin_runtime
            .handle()
            .setup_emulator(snowglass_pluginhost::EmulatorSetupInvocation {
                plugin: request.plugin,
                arguments,
                configuration: request.configuration,
            })
            .await
            .map_err(|error| error.to_string())?;
        plugin_launch_options(plan, &request.viewer)
    }
}
