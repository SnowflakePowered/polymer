//! The shell thread. It owns the Blitz document and its Deno realm, runs
//! Deno's event loop on its own Tokio runtime, and paints when the document
//! changes or animates.

mod input;

use super::{
    ShellOutbox, ShellSessionFactory, ShellSource,
    document::SharedShellDocument,
    protocol::{ShellChannel, ShellCommand, ShellEvent},
    runtime::{PolymerNotificationSender, ShellRuntime, ShellRuntimeOptions},
};
use std::{
    cell::RefCell,
    error::Error,
    future::Future,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc::UnboundedReceiver, oneshot};

/// V8 runs with `--stack-size=4096` (KiB), so its thread needs the same
/// headroom the window thread's 8 MiB stack provided.
const SHELL_STACK_SIZE: usize = 8 << 20;
/// Lower bound between two frames, so a burst of script mutations paints
/// once instead of once per task.
const MIN_FRAME_SPACING: Duration = Duration::from_millis(4);

pub(super) struct ShellStart {
    pub factory: ShellSessionFactory,
    pub source: ShellSource,
    pub commands: polymer_runtime::FrontendCommands,
    pub outbox: ShellOutbox,
}

pub(super) struct ShellStarted {
    pub notifications: PolymerNotificationSender,
    pub inspector_address: std::net::SocketAddr,
}

type StartReply = oneshot::Sender<Result<ShellStarted, String>>;
type StartReceiver = oneshot::Receiver<Result<ShellStarted, String>>;

pub(super) struct ShellHost {
    document: SharedShellDocument,
    runtime: ShellRuntime,
    commands: UnboundedReceiver<ShellCommand>,
    outbox: ShellOutbox,
    frame_timer: Pin<Box<tokio::time::Sleep>>,
    last_frame: Instant,
    paint_requested: bool,
    reload_reported: bool,
}

impl ShellHost {
    pub(super) fn spawn(
        start: ShellStart,
        channel: ShellChannel,
        commands: UnboundedReceiver<ShellCommand>,
    ) -> std::io::Result<(std::thread::JoinHandle<()>, StartReceiver)> {
        let (ready, started) = oneshot::channel();
        let thread = std::thread::Builder::new()
            .name("polymer-shell".into())
            .stack_size(SHELL_STACK_SIZE)
            .spawn(move || Self::run(start, channel, commands, ready))?;
        Ok((thread, started))
    }

    fn run(
        start: ShellStart,
        channel: ShellChannel,
        commands: UnboundedReceiver<ShellCommand>,
        ready: StartReply,
    ) {
        let executor = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(executor) => executor,
            Err(error) => {
                let _ = ready.send(Err(error.to_string()));
                return;
            }
        };
        executor.block_on(async move {
            match Self::start(start, channel, commands).await {
                Ok((mut host, started)) => {
                    let _ = ready.send(Ok(started));
                    std::future::poll_fn(|cx| host.poll(cx)).await;
                }
                Err(error) => {
                    let _ = ready.send(Err(error.to_string()));
                }
            }
        });
    }

    async fn start(
        start: ShellStart,
        channel: ShellChannel,
        commands: UnboundedReceiver<ShellCommand>,
    ) -> Result<(Self, ShellStarted), Box<dyn Error>> {
        let ShellStart {
            factory,
            source,
            commands: frontend,
            outbox,
        } = start;
        let document = SharedShellDocument(Rc::new(RefCell::new(factory.document.create(
            &source,
            channel,
            outbox.clone(),
        ))));
        let (runtime, notifications) = ShellRuntime::start(
            document.clone(),
            ShellRuntimeOptions {
                debug: factory.debug,
                html: source.html,
                source_name: source.base_url,
                commands: frontend,
                plugin_runtime: factory.plugin_runtime,
                local_storage: factory.local_storage,
                library: factory.library,
                input_sources: factory.input_sources,
                controller_api: factory.controller_api,
            },
        )
        .await?;
        let started = ShellStarted {
            notifications,
            inspector_address: runtime.inspector_address(),
        };
        let host = Self {
            document,
            runtime,
            commands,
            outbox,
            frame_timer: Box::pin(tokio::time::sleep(Duration::ZERO)),
            last_frame: Instant::now() - MIN_FRAME_SPACING,
            paint_requested: true,
            reload_reported: false,
        };
        Ok((host, started))
    }

    /// One turn of the shell thread: commands, one event-loop pass, and at
    /// most one paint. Painting publishes layout to script, so it wakes the
    /// next turn to poll the event loop again after handling commands.
    fn poll(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        if self.receive(cx) {
            return Poll::Ready(());
        }
        self.runtime.poll_event_loop(cx);
        self.report_reload();
        if self.paint(cx) {
            cx.waker().wake_by_ref();
        }
        Poll::Pending
    }

    /// Handles every queued command. Consecutive pointer moves collapse into
    /// the newest one, which settles every move it replaced and carries their
    /// positions as coalesced events. Returns true once the window thread
    /// asks the shell to stop.
    fn receive(&mut self, cx: &mut Context<'_>) -> bool {
        let mut batch = Vec::new();
        loop {
            match self.commands.poll_recv(cx) {
                Poll::Ready(Some(command)) => batch.push(command),
                Poll::Ready(None) => return true,
                Poll::Pending => break,
            }
        }
        let mut replaced = Vec::new();
        let mut coalesced = Vec::new();
        let mut batch = batch.into_iter().peekable();
        while let Some(command) = batch.next() {
            match command {
                ShellCommand::Input { sequence, input } => {
                    let superseded = input.pointer_move().filter(|_| {
                        batch.peek().is_some_and(|next| {
                            matches!(next, ShellCommand::Input { input, .. } if input.pointer_move().is_some())
                        })
                    });
                    if let Some(sample) = superseded {
                        replaced.extend(sequence);
                        coalesced.push(sample);
                        continue;
                    }
                    let outcome = self.dispatch(input, std::mem::take(&mut coalesced));
                    for sequence in replaced.drain(..).chain(sequence) {
                        self.outbox
                            .send(ShellEvent::InputSettled { sequence, outcome });
                    }
                }
                ShellCommand::Shutdown => return true,
                command => self.handle(command),
            }
        }
        false
    }

    fn handle(&mut self, command: ShellCommand) {
        match command {
            ShellCommand::Resize {
                width,
                height,
                scale_factor,
            } => self
                .document
                .resize(&mut self.runtime, width, height, scale_factor),
            ShellCommand::Invalidate => self.document.0.borrow_mut().invalidate(),
            ShellCommand::ValidateLaunch { options, reply } => {
                let _ = reply.send(self.invalid_attachment(options));
            }
            ShellCommand::Inspect { dispatch, reply } => {
                let _ = reply.send(Ok(dispatch.handle(&self.document.0.borrow())));
            }
            ShellCommand::Media(signal) => {
                let events = self.document.0.borrow_mut().media_signal(signal);
                if let Some(events) = events {
                    self.runtime.dispatch_media(events);
                }
            }
            ShellCommand::Wake => self.paint_requested = true,
            ShellCommand::Input { .. } | ShellCommand::Shutdown => {}
        }
    }

    fn invalid_attachment(&self, options: serde_json::Value) -> Option<u32> {
        let plan = serde_json::from_value::<crate::launch::AttachedLaunchPlan>(options).ok()?;
        let document = self.document.0.borrow();
        plan.surfaces
            .iter()
            .find(|surface| !document.validates_attachment(surface.attach, surface.index))
            .map(|surface| surface.index)
    }

    fn report_reload(&mut self) {
        if !self.reload_reported && self.runtime.reload_requested() {
            self.reload_reported = true;
            self.outbox.send(ShellEvent::ReloadRequested);
        }
    }

    /// When the next frame should be painted: promptly after a change, no
    /// sooner than `MIN_FRAME_SPACING` after the last frame, and on Blitz's
    /// animation clock while it animates.
    fn paint_due(&self, now: Instant) -> Option<Instant> {
        let document = self.document.0.borrow();
        let changed = (self.paint_requested || document.needs_paint())
            .then(|| (self.last_frame + MIN_FRAME_SPACING).max(now));
        match (changed, document.next_animation_frame()) {
            (Some(changed), Some(animation)) => Some(changed.min(animation)),
            (changed, animation) => changed.or(animation),
        }
    }

    /// Paints when a frame is due, otherwise arms the frame timer. Returns
    /// true when it ran script, which needs another event-loop turn.
    fn paint(&mut self, cx: &mut Context<'_>) -> bool {
        let Some(due) = self.paint_due(Instant::now()) else {
            return false;
        };
        if due > Instant::now() {
            self.frame_timer
                .as_mut()
                .reset(tokio::time::Instant::from_std(due));
            if self.frame_timer.as_mut().poll(cx).is_pending() {
                return false;
            }
        }
        self.paint_requested = false;
        self.last_frame = Instant::now();
        if let Some(frame) = self.document.produce_frame(&mut self.runtime) {
            let structure_revision = self.document.0.borrow().dom_structure_revision();
            self.outbox.send(ShellEvent::Frame {
                frame,
                structure_revision,
            });
        }
        true
    }
}
