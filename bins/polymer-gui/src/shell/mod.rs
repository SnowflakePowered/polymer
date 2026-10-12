//! The shell document, its Deno realm, and the thread that drives them.
//!
//! The document, Blitz layout and paint, and the Deno event loop all live on
//! a dedicated shell thread so script timers, sockets, and long tasks never
//! hold the window thread. `ShellSession` is the window thread's handle: it
//! sends input and window changes, and drains published frames and input
//! outcomes when the shell thread posts `WindowCommand::ShellUpdated`.

pub(crate) mod document;
mod host;
pub(crate) mod protocol;
pub(crate) mod runtime;

use crate::{
    app::UserEvent,
    window_manager::{EventLoopProxy, WindowCommand},
};
use document::ShellDocumentFactory;
use protocol::{ShellChannel, ShellCommand, ShellEvent, ShellInput};
use std::{
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, Sender},
    },
    thread::JoinHandle,
};
use tokio::sync::oneshot;

/// Delivers shell events to the window thread, posting at most one wake per
/// drain so a burst of frames and input outcomes costs one message.
#[derive(Clone)]
struct ShellOutbox {
    events: Sender<ShellEvent>,
    signalled: Arc<AtomicBool>,
    proxy: EventLoopProxy<UserEvent>,
}

impl ShellOutbox {
    fn send(&self, event: ShellEvent) {
        if self.events.send(event).is_err() {
            return;
        }
        if !self.signalled.swap(true, Ordering::AcqRel) {
            let _ = self
                .proxy
                .send_event(UserEvent::Window(WindowCommand::ShellUpdated));
        }
    }
}

pub(crate) struct ShellSession {
    slot: ShellDocumentSlot,
}

enum ShellDocumentSlot {
    Awaiting(Box<ShellSessionFactory>),
    Active(Box<ActiveShell>),
    Disabled,
}

struct ActiveShell {
    channel: ShellChannel,
    events: Receiver<ShellEvent>,
    signalled: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    factory: ShellSessionFactory,
    inspector_address: std::net::SocketAddr,
    reload_requested: bool,
}

impl Drop for ActiveShell {
    fn drop(&mut self) {
        self.channel.send(ShellCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Where the shell document is loaded from and the window it starts in.
pub(crate) struct ShellSource {
    pub html: String,
    pub base_url: String,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f32,
}

impl ShellSession {
    pub fn new(factory: ShellSessionFactory) -> Self {
        Self {
            slot: ShellDocumentSlot::Awaiting(Box::new(factory)),
        }
    }

    fn active(&self) -> Option<&ActiveShell> {
        match &self.slot {
            ShellDocumentSlot::Active(active) => Some(active),
            ShellDocumentSlot::Awaiting(_) | ShellDocumentSlot::Disabled => None,
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active().is_some()
    }

    pub(crate) fn channel(&self) -> Option<ShellChannel> {
        self.active().map(|active| active.channel.clone())
    }

    pub(crate) fn inspector_address(&self) -> Option<std::net::SocketAddr> {
        self.active().map(|active| active.inspector_address)
    }

    /// Starts the shell thread and waits for its document scripts to run.
    pub async fn activate(
        &mut self,
        source: ShellSource,
        commands: polymer_runtime::FrontendCommands,
    ) -> Result<Option<crate::shell::runtime::PolymerNotificationSender>, Box<dyn Error>> {
        let slot = std::mem::replace(&mut self.slot, ShellDocumentSlot::Disabled);
        let ShellDocumentSlot::Awaiting(factory) = slot else {
            self.slot = slot;
            return Ok(None);
        };
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let channel = ShellChannel(sender);
        let (events_sender, events) = std::sync::mpsc::channel();
        let signalled = Arc::new(AtomicBool::new(false));
        let outbox = ShellOutbox {
            events: events_sender,
            signalled: signalled.clone(),
            proxy: factory.event_proxy.clone(),
        };
        let (thread, started) = host::ShellHost::spawn(
            host::ShellStart {
                factory: (*factory).clone(),
                source,
                commands,
                outbox,
            },
            channel.clone(),
            receiver,
        )?;
        let started = match started.await {
            Ok(Ok(started)) => started,
            Ok(Err(error)) => {
                let _ = thread.join();
                return Err(error.into());
            }
            Err(_) => {
                let _ = thread.join();
                return Err("the shell thread exited during startup".into());
            }
        };
        self.slot = ShellDocumentSlot::Active(Box::new(ActiveShell {
            channel,
            events,
            signalled,
            thread: Some(thread),
            factory: *factory,
            inspector_address: started.inspector_address,
            reload_requested: false,
        }));
        Ok(Some(started.notifications))
    }

    pub fn send(&self, command: ShellCommand) {
        if let Some(active) = self.active() {
            active.channel.send(command);
        }
    }

    pub fn input(&self, sequence: Option<u64>, input: ShellInput) {
        self.send(ShellCommand::Input { sequence, input });
    }

    /// Collects every event the shell thread published since the last drain.
    pub fn drain(&mut self) -> Vec<ShellEvent> {
        let ShellDocumentSlot::Active(active) = &mut self.slot else {
            return Vec::new();
        };
        active.signalled.store(false, Ordering::Release);
        let mut events = Vec::new();
        for event in active.events.try_iter() {
            if matches!(event, ShellEvent::ReloadRequested) {
                active.reload_requested = true;
            } else {
                events.push(event);
            }
        }
        events
    }

    /// Asks the shell thread which launch surface, if any, does not attach
    /// to its tracked-surface element. Resolves to `None` without a shell.
    pub fn validate_launch(&self, options: serde_json::Value) -> oneshot::Receiver<Option<u32>> {
        let (reply, response) = oneshot::channel();
        match self.active() {
            Some(active) => active
                .channel
                .send(ShellCommand::ValidateLaunch { options, reply }),
            None => {
                let _ = reply.send(None);
            }
        }
        response
    }

    pub fn reload_pending(&self) -> bool {
        self.active().is_some_and(|active| active.reload_requested)
    }

    /// Stops the current shell thread when script asked for a reload, leaving
    /// the session ready to activate a fresh document.
    pub fn take_reload(&mut self) -> bool {
        if !self.reload_pending() {
            return false;
        }
        let ShellDocumentSlot::Active(active) =
            std::mem::replace(&mut self.slot, ShellDocumentSlot::Disabled)
        else {
            unreachable!();
        };
        self.slot = ShellDocumentSlot::Awaiting(Box::new(active.factory.clone()));
        true
    }

    pub fn disable(&mut self) {
        if matches!(self.slot, ShellDocumentSlot::Awaiting(_)) {
            self.slot = ShellDocumentSlot::Disabled;
        }
    }
}

#[derive(Clone)]
pub(crate) struct ShellSessionFactory {
    debug: bool,
    event_proxy: EventLoopProxy<UserEvent>,
    document: ShellDocumentFactory,
    plugin_runtime: std::sync::Arc<polymer_pluginhost::runtime::PluginRuntime>,
    local_storage: Option<polymer_deno::webstorage::LocalStorage>,
    library: Option<polymer_runtime::LibraryService>,
    input_sources: polymer_input::InputHub,
    controller_api: polymer_runtime::ControllerApiState,
}

impl ShellSessionFactory {
    pub(crate) fn new(
        debug: bool,
        plugin_registry: polymer_pluginhost::loading::PluginRegistry,
        directories: polymer_library::Directories,
        local_storage: Option<polymer_deno::webstorage::LocalStorage>,
        library: Option<polymer_runtime::LibraryService>,
        jobs: Option<std::sync::Arc<polymer_library::jobs::JobStore>>,
        event_proxy: EventLoopProxy<UserEvent>,
    ) -> Self {
        let plugin_runtime = polymer_pluginhost::runtime::PluginRuntime::new(
            plugin_registry,
            directories,
            polymer_pluginhost::runtime::PluginRuntimeOptions {
                jobs,
                ..Default::default()
            },
        );
        Self {
            debug,
            document: ShellDocumentFactory::new(event_proxy.clone()),
            event_proxy,
            plugin_runtime: std::sync::Arc::new(plugin_runtime),
            local_storage,
            library,
            input_sources: polymer_input::InputHub::default(),
            controller_api: polymer_runtime::ControllerApiState::default(),
        }
    }

    pub(crate) fn input_sources(&self) -> polymer_input::InputHub {
        self.input_sources.clone()
    }

    pub(crate) fn controller_api(&self) -> polymer_runtime::ControllerApiState {
        self.controller_api.clone()
    }
}
