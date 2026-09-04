use super::{InvocationControl, PluginInvocationError, PluginLifecycle};
use crate::plugin_runtime::worker::{WorkerOptions, create_worker_runtime};
use crate::{EmulatorLaunchArguments, EmulatorLaunchInvocation, EmulatorPluginInstance};
use snowglass_command::command_handler;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

struct IsolateDeadline {
    completed: std::sync::mpsc::SyncSender<()>,
    expired: Arc<AtomicBool>,
    watchdog: thread::JoinHandle<()>,
}

impl IsolateDeadline {
    fn start(
        isolate: deno_core::v8::IsolateHandle,
        timeout: std::time::Duration,
    ) -> Result<Self, PluginInvocationError> {
        let (completed, receiver) = std::sync::mpsc::sync_channel(1);
        let expired = Arc::new(AtomicBool::new(false));
        let watchdog_expired = expired.clone();
        let watchdog = thread::Builder::new()
            .name("snowglass-plugin-deadline".to_owned())
            .spawn(move || {
                if receiver.recv_timeout(timeout).is_err() {
                    watchdog_expired.store(true, Ordering::Release);
                    isolate.terminate_execution();
                }
            })
            .map_err(PluginInvocationError::worker)?;
        Ok(Self {
            completed,
            expired,
            watchdog,
        })
    }

    fn finish(self, runtime: &mut deno_core::JsRuntime) -> bool {
        let _ = self.completed.send(());
        let _ = self.watchdog.join();
        let expired = self.expired.load(Ordering::Acquire);
        if expired {
            runtime.v8_isolate().cancel_terminate_execution();
        }
        expired
    }
}
pub(super) fn start_controller_session_worker(
    invocation: EmulatorLaunchInvocation,
    worker: WorkerOptions,
    lifecycle: Arc<PluginLifecycle>,
    control: Arc<InvocationControl>,
) -> Result<(EmulatorLaunchArguments, Option<crate::ControllerSession>), PluginInvocationError> {
    let (commands, command_receiver) = std::sync::mpsc::channel();
    let (started, startup) = std::sync::mpsc::sync_channel(1);
    thread::Builder::new()
        .name(format!(
            "snowglass-plugin-session-{}",
            invocation.plugin.id()
        ))
        .spawn(move || {
            let result = ControllerSessionWorker::start(
                invocation,
                worker,
                lifecycle,
                control,
                command_receiver,
                started,
            )
            .map(|worker| {
                if let Some(worker) = worker {
                    worker.run();
                }
            });
            if let Err(error) = result {
                tracing::error!("snowglass: controller plugin session failed: {error}");
            }
        })
        .map_err(PluginInvocationError::worker)?;
    let (launch, retained) = startup.recv().map_err(|_| {
        PluginInvocationError::worker("controller plugin session failed to start")
    })??;
    let session = retained.then(|| crate::ControllerSession::new(commands));
    Ok((launch, session))
}

struct ControllerSessionWorker {
    tokio: tokio::runtime::Runtime,
    runtime: snowglass_deno::Runtime,
    plugin: Arc<dyn EmulatorPluginInstance>,
    console: String,
    configuration_id: crate::configuration::ConfigurationId,
    commands: std::sync::mpsc::Receiver<crate::controller::ControllerSessionCommand>,
}

impl ControllerSessionWorker {
    fn start(
        invocation: EmulatorLaunchInvocation,
        worker: WorkerOptions,
        lifecycle: Arc<PluginLifecycle>,
        control: Arc<InvocationControl>,
        commands: std::sync::mpsc::Receiver<crate::controller::ControllerSessionCommand>,
        started: std::sync::mpsc::SyncSender<
            Result<(EmulatorLaunchArguments, bool), PluginInvocationError>,
        >,
    ) -> Result<Option<Self>, PluginInvocationError> {
        let entrypoint = invocation.plugin.entrypoint().to_string();
        let tokio = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(PluginInvocationError::worker)?;
        let mut runtime = {
            let _runtime_context = tokio.enter();
            create_worker_runtime(&entrypoint, worker, lifecycle.storage.clone())?
        };
        let isolate = runtime.js_runtime_mut().v8_isolate().thread_safe_handle();
        if control.install_isolate(isolate) {
            let _ = started.send(Err(PluginInvocationError::cancelled()));
            return Ok(None);
        }
        if let Err(error) = tokio.block_on(lifecycle.load(&mut runtime, &entrypoint)) {
            let _ = started.send(Err(error));
            return Ok(None);
        }
        let plugin = invocation.plugin.clone();
        let console = invocation.arguments["console"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let topology = invocation.topology.clone();
        let retained = match tokio.block_on(crate::deno::execute_retained_emulator(
            runtime.js_runtime_mut(),
            invocation,
        )) {
            Ok(retained) => retained,
            Err(error) => {
                let _ = started.send(Err(PluginInvocationError::worker(error)));
                return Ok(None);
            }
        };
        let retained_session = retained.retained;
        let configuration_id = retained.configuration_id;
        let mut launch = retained.launch;
        let applied_topology = launch.controller_topology.clone().unwrap_or(topology);
        tracing::info!(
            plugin = %plugin.id(),
            thread = std::thread::current().name().unwrap_or("unnamed"),
            applied = %applied_topology.assignment_diagnostics(),
            "snowglass: controller plugin session initialized"
        );
        launch.controller_topology = Some(applied_topology.clone());
        if started.send(Ok((launch, retained_session))).is_err() || !retained_session {
            crate::deno::discard_configuration(runtime.js_runtime_mut(), configuration_id);
            return Ok(None);
        }
        Ok(Some(Self {
            tokio,
            runtime,
            plugin,
            console,
            configuration_id,
            commands,
        }))
    }

    fn run(mut self) {
        while let Ok(command) = self.commands.recv() {
            if command.dispatch(&mut self) {
                break;
            }
        }
        crate::automation::close(self.runtime.js_runtime_mut());
        crate::actions::close(self.runtime.js_runtime_mut());
        crate::deno::discard_configuration(self.runtime.js_runtime_mut(), self.configuration_id);
    }

    fn apply_change(
        &mut self,
        change: &snowglass_controllers::ControllerTopologyChange,
    ) -> Result<snowglass_controllers::ControllerTopology, PluginInvocationError> {
        let checkpoint = crate::deno::checkpoint_configuration(
            self.runtime.js_runtime_mut(),
            self.configuration_id,
        )
        .map_err(PluginInvocationError::worker)?;
        let result = self.apply_change_inner(change);
        if result.is_err() {
            crate::deno::restore_configuration(
                self.runtime.js_runtime_mut(),
                self.configuration_id,
                checkpoint,
            );
            if let Err(error) =
                crate::deno::refresh_configuration_mounts(self.runtime.js_runtime_mut())
                    .map_err(PluginInvocationError::worker)
                    .and_then(|()| {
                        crate::filesystem::flush_staged(self.runtime.js_runtime_mut())
                            .map_err(PluginInvocationError::worker)
                    })
            {
                tracing::error!(
                    plugin = %self.plugin.id(),
                    "snowglass: restoring controller configuration files failed: {error}"
                );
            }
        }
        result
    }

    fn apply_change_inner(
        &mut self,
        change: &snowglass_controllers::ControllerTopologyChange,
    ) -> Result<snowglass_controllers::ControllerTopology, PluginInvocationError> {
        let assignments = change
            .requested
            .port_assignments()
            .map_err(PluginInvocationError::worker)?;
        let configuration = self
            .plugin
            .materialize(&self.console, assignments)
            .map_err(PluginInvocationError::worker)?;
        crate::deno::rematerialize_configuration(
            self.runtime.js_runtime_mut(),
            self.configuration_id,
            configuration,
            &change.requested,
        )
        .map_err(PluginInvocationError::worker)?;
        let topology = self.execute_callback(change)?;
        crate::filesystem::flush_staged(self.runtime.js_runtime_mut())
            .map_err(PluginInvocationError::worker)?;
        Ok(topology)
    }

    fn execute_callback(
        &mut self,
        change: &snowglass_controllers::ControllerTopologyChange,
    ) -> Result<snowglass_controllers::ControllerTopology, PluginInvocationError> {
        const TIMEOUT: std::time::Duration =
            snowglass_controllers::DEFAULT_CONTROLLER_TOPOLOGY_TRANSACTION_TIMEOUT;
        const TIMEOUT_ERROR: &str = "onControllerChanged exceeded its 60 second deadline";
        let deadline = IsolateDeadline::start(
            self.runtime
                .js_runtime_mut()
                .v8_isolate()
                .thread_safe_handle(),
            TIMEOUT,
        )?;
        let callback = self.tokio.block_on(async {
            tokio::time::timeout(
                TIMEOUT,
                crate::deno::execute_controller_change(
                    self.runtime.js_runtime_mut(),
                    &change.requested,
                    &change.previous,
                ),
            )
            .await
            .map_err(|_| PluginInvocationError::worker(TIMEOUT_ERROR))?
            .map_err(PluginInvocationError::worker)
        });
        if deadline.finish(self.runtime.js_runtime_mut()) {
            return Err(PluginInvocationError::worker(TIMEOUT_ERROR));
        }
        callback
    }
}

#[command_handler]
impl crate::controller::ControllerSessionCommandHandler for ControllerSessionWorker {
    type Output<'handler>
        = bool
    where
        Self: 'handler;

    fn attach_automation(
        &mut self,
        session: snowglass_automation::AutomationSession,
    ) -> Result<(), String> {
        crate::automation::attach(self.runtime.js_runtime_mut(), session);
        Ok(())
    }

    fn attach_action_sink(&mut self, sink: crate::actions::ActionSink) -> Result<(), String> {
        crate::actions::attach(self.runtime.js_runtime_mut(), sink);
        Ok(())
    }

    fn change(
        &mut self,
        change: snowglass_controllers::ControllerTopologyChange,
    ) -> Result<snowglass_controllers::ControllerTopology, String> {
        let started = std::time::Instant::now();
        let thread = std::thread::current();
        tracing::info!(
            plugin = %self.plugin.id(),
            thread = thread.name().unwrap_or("unnamed"),
            previous = %change.previous.assignment_diagnostics(),
            requested = %change.requested.assignment_diagnostics(),
            "snowglass: controller plugin callback started"
        );
        match self.apply_change(&change) {
            Ok(applied) => {
                tracing::info!(
                    plugin = %self.plugin.id(),
                    thread = thread.name().unwrap_or("unnamed"),
                    elapsed_ms = started.elapsed().as_millis(),
                    applied = %applied.assignment_diagnostics(),
                    "snowglass: controller plugin callback completed"
                );
                Ok(applied)
            }
            Err(error) => {
                crate::filesystem::discard_staged(self.runtime.js_runtime_mut());
                tracing::error!(
                    plugin = %self.plugin.id(),
                    thread = thread.name().unwrap_or("unnamed"),
                    elapsed_ms = started.elapsed().as_millis(),
                    requested = %change.requested.assignment_diagnostics(),
                    "snowglass: onControllerChanged failed: {error}"
                );
                Err(error.to_string())
            }
        }
    }

    fn pause_resume_with_timeout(
        &mut self,
        state: crate::PauseState,
        timeout: std::time::Duration,
    ) -> Result<(), String> {
        let started = std::time::Instant::now();
        let thread = std::thread::current();
        tracing::info!(
            plugin = %self.plugin.id(),
            ?state,
            thread = thread.name().unwrap_or("unnamed"),
            "snowglass: pause/resume plugin callback started"
        );
        let result = self.tokio.block_on(async {
            tokio::time::timeout(
                timeout,
                crate::deno::execute_pause_resume(self.runtime.js_runtime_mut(), state),
            )
            .await
            .map_err(|_| {
                format!(
                    "onPauseResume exceeded its {}ms deadline",
                    timeout.as_millis()
                )
            })?
            .map_err(|error| error.to_string())
        });
        if let Err(error) = &result {
            tracing::error!("snowglass: onPauseResume failed: {error}");
        } else {
            tracing::info!(
                plugin = %self.plugin.id(),
                ?state,
                thread = thread.name().unwrap_or("unnamed"),
                elapsed_ms = started.elapsed().as_millis(),
                "snowglass: pause/resume plugin callback completed"
            );
        }
        result
    }

    fn dispatch_state_action(
        &mut self,
        slot: u32,
        save: bool,
        timeout: std::time::Duration,
    ) -> Result<(), String> {
        let callback = if save { "onStateSave" } else { "onStateResume" };
        let timeout_error = format!("{callback} exceeded its {}ms deadline", timeout.as_millis());
        let deadline = IsolateDeadline::start(
            self.runtime
                .js_runtime_mut()
                .v8_isolate()
                .thread_safe_handle(),
            timeout,
        )
        .map_err(|error| error.to_string())?;
        let result = self.tokio.block_on(async {
            tokio::time::timeout(
                timeout,
                crate::deno::execute_state_action(self.runtime.js_runtime_mut(), slot, save),
            )
            .await
            .map_err(|_| timeout_error.clone())?
            .map_err(|error| error.to_string())
        });
        let result = if deadline.finish(self.runtime.js_runtime_mut()) {
            Err(timeout_error)
        } else {
            result
        };
        if let Err(error) = &result {
            tracing::error!(plugin = %self.plugin.id(), slot, "snowglass: {callback} failed: {error}");
        }
        result
    }

    fn quit_with_timeout(
        &mut self,
        timeout: std::time::Duration,
    ) -> Result<Option<Vec<u8>>, String> {
        let timeout_error = format!("onQuit exceeded its {}ms deadline", timeout.as_millis());
        let deadline = IsolateDeadline::start(
            self.runtime
                .js_runtime_mut()
                .v8_isolate()
                .thread_safe_handle(),
            timeout,
        )
        .map_err(|error| error.to_string())?;
        let result = self.tokio.block_on(async {
            tokio::time::timeout(
                timeout,
                crate::deno::execute_quit(self.runtime.js_runtime_mut()),
            )
            .await
            .map_err(|_| timeout_error.clone())?
            .map_err(|error| error.to_string())
        });
        let result = if deadline.finish(self.runtime.js_runtime_mut()) {
            Err(timeout_error)
        } else {
            result
        };
        if let Err(error) = &result {
            tracing::error!(plugin = %self.plugin.id(), "snowglass: onQuit failed: {error}");
        }
        result
    }

    fn stop<'handler>(&'handler mut self) -> Self::Output<'handler> {
        true
    }
}
