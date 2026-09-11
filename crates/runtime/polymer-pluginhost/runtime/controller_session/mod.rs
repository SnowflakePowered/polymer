mod callbacks;
mod deadline;
mod event_loop;

use super::worker::{WorkerOptions, create_worker_runtime};
use super::{InvocationControl, PluginInvocationError, PluginLifecycle};
use crate::{
    controller::ControllerSession,
    emulator::{EmulatorLaunchArguments, EmulatorLaunchInvocation, EmulatorPluginInstance},
};
use actix::{Actor, AsyncContext, Context, System};
use std::sync::Arc;

impl ControllerActor {
    pub(super) fn spawn(
        invocation: EmulatorLaunchInvocation,
        worker: WorkerOptions,
        lifecycle: Arc<PluginLifecycle>,
        control: Arc<InvocationControl>,
    ) -> Result<(EmulatorLaunchArguments, Option<ControllerSession>), PluginInvocationError> {
        let (started, startup) = std::sync::mpsc::sync_channel(1);
        let actor_control = control.clone();
        let thread = std::thread::Builder::new()
            .name("polymer-plugin-actor".to_owned())
            .spawn(move || {
                // A current-thread Actix system keeps every V8 access and drop on this thread.
                let system = System::new();
                let initialized = system.block_on(ControllerActor::initialize(
                    invocation,
                    worker,
                    lifecycle,
                    actor_control.clone(),
                ));
                let (actor, launch, retained) = match initialized {
                    Ok(value) => value,
                    Err(error) => {
                        let _ = started.send(Err(error));
                        return Ok(());
                    }
                };
                if !retained {
                    drop(actor);
                    let _ = started.send(Ok((launch, None)));
                    return Ok(());
                }
                let address = system.block_on(async move {
                    let address = ControllerActor::create(move |ctx| {
                        // Zero is Actix's unbounded mailbox; no additional admission layer.
                        ctx.set_mailbox_capacity(0);
                        actor
                    });
                    actix::spawn(async move {
                        if !actor_control.is_cancelled() {
                            actor_control.cancelled_notification().await;
                        }
                        System::current().stop();
                    });
                    address
                });
                if started.send(Ok((launch, Some(address)))).is_err() {
                    return Ok(());
                }
                system.run().map_err(|error| error.to_string())
            })
            .map_err(PluginInvocationError::worker)?;
        let (launch, address) = startup.recv().map_err(PluginInvocationError::worker)??;
        match address {
            Some(client) => Ok((
                launch,
                Some(ControllerSession {
                    client,
                    control,
                    thread: Some(thread),
                }),
            )),
            None => {
                thread
                    .join()
                    .map_err(|_| PluginInvocationError::worker("controller actor thread panicked"))?
                    .map_err(PluginInvocationError::worker)?;
                Ok((launch, None))
            }
        }
    }
}

/// The retained plugin and its !Send Deno isolate live directly in the Actix actor.
pub struct ControllerActor {
    runtime: polymer_deno::Runtime,
    plugin: Arc<dyn EmulatorPluginInstance>,
    console: String,
    configuration_id: crate::configuration::ConfigurationId,
    control: Arc<InvocationControl>,
}

impl ControllerActor {
    async fn initialize(
        invocation: EmulatorLaunchInvocation,
        worker: WorkerOptions,
        lifecycle: Arc<PluginLifecycle>,
        control: Arc<InvocationControl>,
    ) -> Result<(Self, EmulatorLaunchArguments, bool), PluginInvocationError> {
        let entrypoint = invocation.plugin.entrypoint().to_string();
        let mut runtime = create_worker_runtime(&entrypoint, worker, lifecycle.storage.clone())?;
        let isolate = runtime.js_runtime_mut().v8_isolate().thread_safe_handle();
        if control.install_isolate(isolate) {
            return Err(PluginInvocationError::cancelled());
        }
        lifecycle.load(&mut runtime, &entrypoint).await?;
        let plugin = invocation.plugin.clone();
        let console = invocation.arguments["console"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let topology = invocation.topology.clone();
        let retained = crate::emulator::adapter::execute_retained_emulator(
            runtime.js_runtime_mut(),
            invocation,
        )
        .await
        .map_err(PluginInvocationError::worker)?;
        let mut launch = retained.launch;
        let applied_topology = launch.controller_topology.clone().unwrap_or(topology);
        tracing::info!(plugin = %plugin.id(), thread = ?std::thread::current().id(),
            applied = %applied_topology.assignment_diagnostics(),
            "polymer: controller plugin session initialized");
        launch.controller_topology = Some(applied_topology);
        let state = Self {
            runtime,
            plugin,
            console,
            configuration_id: retained.configuration_id,
            control,
        };
        Ok((state, launch, retained.retained))
    }
}

impl Actor for ControllerActor {
    type Context = Context<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        ctx.spawn(event_loop::IdleEventLoop);
    }

    fn stopped(&mut self, _: &mut Self::Context) {
        System::current().stop();
    }
}

impl Drop for ControllerActor {
    fn drop(&mut self) {
        crate::automation::close(self.runtime.js_runtime_mut());
        crate::actions::close(self.runtime.js_runtime_mut());
        crate::emulator::adapter::discard_configuration(
            self.runtime.js_runtime_mut(),
            self.configuration_id,
        );
    }
}
