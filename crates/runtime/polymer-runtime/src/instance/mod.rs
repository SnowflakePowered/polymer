use crate::{
    GraphicsHook, Hooks,
    capture::{self, ConsumerPreference, FrameRouter},
};
use crossfire::{MRx, MTx, mpmc, oneshot::TxOneshot};
use dashmap::{DashMap, DashSet};
use parking_lot::{Mutex, RwLock};
use polymer_input::{HostInputEvent, HostPointerInput};
use polymer_ipc::StreamReady;
use polymer_ipc::input::host::InputIpcListener;
pub use polymer_platform::{AudioState, HotkeyChord, NormalizedVolume, ResizeDimensions};
use polymer_platform::{Guest, GuestInput, GuestWindowId, ProcessExitReceiver};

use polymer_protocol::{
    ExternalImageKind, FrameReady,
    input::{PointerPhase, PointerSource},
};
use std::{
    collections::HashSet,
    fmt,
    path::PathBuf,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use watchable::{TimeoutError, Watchable, Watcher};

type SurfaceEventSender = MTx<mpmc::List<SurfaceEvent>>;

mod config;
mod manager;
mod surface;

pub use config::*;
pub(crate) use manager::*;
pub use surface::*;

pub struct EmulatorInstance {
    process_id: u32,
    lifecycle: Arc<InstanceLifecycle>,
    events: SurfaceEventReceiver,
}

impl EmulatorInstance {
    pub fn launch(
        config: EmulatorLaunchConfig,
        filesystem: &polymer_vfs::MountedFilesystem,
    ) -> Result<Self, LaunchError> {
        Self::launch_with_waker(config, filesystem, || {})
    }

    pub fn launch_with_waker(
        config: EmulatorLaunchConfig,
        filesystem: &polymer_vfs::MountedFilesystem,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, LaunchError> {
        Self::launch_with_input(config, filesystem, wake, |_| Ok(()))
    }

    pub fn launch_with_input(
        config: EmulatorLaunchConfig,
        filesystem: &polymer_vfs::MountedFilesystem,
        wake: impl Fn() + Send + Sync + 'static,
        configure_input: impl FnOnce(InputIpcListener) -> Result<(), String>,
    ) -> Result<Self, LaunchError> {
        config.validate()?;
        let request = config.guest_request(filesystem)?;
        let guest = Arc::new(Mutex::new(None));
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let input = InputIpcListener::bind(&config.session)?;
        if let Err(error) = configure_input(input.clone()) {
            input.shutdown();
            return Err(LaunchError::InvalidConfig(error));
        }
        let (event_sender, event_receiver) = mpmc::unbounded_blocking();
        let surfaces = Arc::new(SurfaceManager::new(
            &config,
            Arc::downgrade(&guest),
            event_sender,
        ));
        let offer_manager = Arc::downgrade(&surfaces);
        let disconnect_manager = Arc::downgrade(&surfaces);
        let callbacks = capture::CaptureCallbacks {
            offer: Arc::new(move |connection, stream, accepted| {
                if let Some(manager) = offer_manager.upgrade() {
                    manager.offer(connection, stream, accepted);
                } else {
                    accepted.send(false);
                }
            }),
            disconnected: Arc::new(move |connection| {
                if let Some(manager) = disconnect_manager.upgrade() {
                    manager.disconnected(connection);
                }
            }),
        };
        let router = match capture::bind(&config.session, config.consumer, callbacks, Some(wake)) {
            Ok(router) => router,
            Err(error) => {
                input.shutdown();
                return Err(error.into());
            }
        };
        surfaces.install_router(router.clone());
        let mut process =
            match crate::launch::GuestPlatform::launch(&request, filesystem, input.clone()) {
                Ok(process) => process,
                Err(error) => {
                    surfaces.shutdown();
                    input.shutdown();
                    return Err(error.into());
                }
            };
        let process_id = process.id().0;
        let wait_handle = match process.subscribe_exit() {
            Ok(wait_handle) => wait_handle,
            Err(error) => {
                let _ = process.terminate_tree_and_wait();
                surfaces.shutdown();
                input.shutdown();
                return Err(error.into());
            }
        };
        *guest.lock() = Some(process);
        let state = Watchable::new(InstanceState::Starting);
        let lifecycle = Arc::new(InstanceLifecycle {
            guest,
            input: input.clone(),
            state: state.clone(),
            surfaces: surfaces.clone(),
            finishing: AtomicBool::new(false),
            monitor: Mutex::new(None),
        });
        state.replace(InstanceState::Running);
        if let Err(error) = lifecycle.start_monitor(wait_handle) {
            lifecycle.finish(InstanceExit::Terminated);
            return Err(error.into());
        }
        Ok(Self {
            process_id,
            lifecycle,
            events: event_receiver,
        })
    }

    pub const fn process_id(&self) -> u32 {
        self.process_id
    }

    pub fn state(&self) -> InstanceState {
        self.lifecycle.state.get()
    }

    pub fn input(&self) -> InputIpcListener {
        self.lifecycle.input.clone()
    }

    pub fn action_endpoint(&self) -> EmulatorActionEndpoint {
        EmulatorActionEndpoint {
            lifecycle: self.lifecycle.clone(),
        }
    }

    pub fn tracked_surface(&self, id: TrackedSurfaceId) -> Option<TrackedSurfaceHandle> {
        self.lifecycle
            .surfaces
            .active
            .get(&id)
            .map(|surface| surface.value().handle.clone())
    }

    pub fn tracked_surfaces(&self) -> Vec<TrackedSurfaceHandle> {
        let mut surfaces = self
            .lifecycle
            .surfaces
            .active
            .iter()
            .map(|surface| surface.value().handle.clone())
            .collect::<Vec<_>>();
        surfaces.sort_by_key(TrackedSurfaceHandle::id);
        surfaces
    }

    pub fn subscribe_surfaces(&self) -> SurfaceEventReceiver {
        self.events.clone()
    }

    pub fn audio_state(&self) -> AudioState {
        self.lifecycle
            .guest
            .lock()
            .as_ref()
            .and_then(|guest| guest.audio_state().ok())
            .unwrap_or_default()
    }

    pub fn set_volume(&self, volume: NormalizedVolume) {
        if let Some(guest) = self.lifecycle.guest.lock().as_ref()
            && let Err(error) = guest.set_volume(volume)
        {
            tracing::warn!("polymer: setting guest volume failed: {error}");
        }
    }

    pub fn set_muted(&self, muted: bool) {
        if let Some(guest) = self.lifecycle.guest.lock().as_ref()
            && let Err(error) = guest.set_muted(muted)
        {
            tracing::warn!("polymer: setting guest mute failed: {error}");
        }
    }

    pub fn close(&mut self) -> InstanceExit {
        self.lifecycle.finish(InstanceExit::Terminated);
        let exit = self.lifecycle.wait_blocking();
        self.lifecycle.join_monitor();
        exit
    }

    pub async fn wait(&mut self) -> InstanceExit {
        let states = self.lifecycle.state.watch();
        states.mark_read();
        loop {
            if let Some(exit) = terminal_exit(&self.lifecycle.state) {
                self.lifecycle.join_monitor();
                return exit;
            }
            if states.next_value_async().await.is_err() {
                let exit = terminal_exit(&self.lifecycle.state).unwrap_or(InstanceExit::Natural);
                self.lifecycle.join_monitor();
                return exit;
            }
        }
    }
}

impl Drop for EmulatorInstance {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn terminal_exit(state: &Watchable<InstanceState>) -> Option<InstanceExit> {
    match state.get() {
        InstanceState::Exited(exit) => Some(exit.clone()),
        _ => None,
    }
}

struct InstanceLifecycle {
    guest: Arc<Mutex<Option<Box<dyn Guest>>>>,
    input: InputIpcListener,
    state: Watchable<InstanceState>,
    surfaces: Arc<SurfaceManager>,
    finishing: AtomicBool,
    monitor: Mutex<Option<JoinHandle<()>>>,
}

impl InstanceLifecycle {
    fn start_monitor(self: &Arc<Self>, wait: ProcessExitReceiver) -> std::io::Result<()> {
        let lifecycle = Arc::downgrade(self);
        let worker = thread::Builder::new()
            .name("polymer-process-wait".to_owned())
            .spawn(move || {
                let exit = match wait.recv() {
                    Ok(Ok(exit)) => {
                        tracing::info!(?exit, "polymer: emulator process exited");
                        InstanceExit::Natural
                    }
                    Ok(Err(error)) => InstanceExit::Failed(Arc::from(error.to_string())),
                    Err(error) => InstanceExit::Failed(Arc::from(error.to_string())),
                };
                if let Some(lifecycle) = lifecycle.upgrade() {
                    lifecycle.finish(exit);
                }
            })?;
        *self.monitor.lock() = Some(worker);
        Ok(())
    }

    fn finish(&self, exit: InstanceExit) {
        if self.finishing.swap(true, Ordering::AcqRel) {
            return;
        }
        self.state.replace(InstanceState::Closing);
        // End the producer before waiting for capture/audio transports to
        // shut down. Those workers can be blocked in target-owned IPC while
        // the emulator is alive, which previously prevented close() from ever
        // reaching the process termination step.
        if let Some(mut process) = self.guest.lock().take() {
            let _ = process.terminate_tree_and_wait();
        }
        self.surfaces.shutdown();
        self.input.shutdown();
        self.state.replace(InstanceState::Exited(exit));
        self.state.shutdown();
    }

    fn wait_blocking(&self) -> InstanceExit {
        let watcher = self.state.watch();
        watcher.mark_read();
        loop {
            if let Some(exit) = terminal_exit(&self.state) {
                return exit;
            }
            if watcher.next_value().is_err() {
                return terminal_exit(&self.state).unwrap_or(InstanceExit::Natural);
            }
        }
    }

    fn join_monitor(&self) {
        if let Some(worker) = self.monitor.lock().take() {
            let _ = worker.join();
        }
    }
}

impl Drop for InstanceLifecycle {
    fn drop(&mut self) {
        self.finish(InstanceExit::Terminated);
        self.join_monitor();
    }
}
