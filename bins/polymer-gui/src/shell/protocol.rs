//! Messages exchanged between the window thread and the shell thread.

use crate::app::UserEvent;
use crate::inspector::elements::{Dispatch, ProtocolError, Reply};
use crate::shell::document::{GameViewStyle, MediaSignal};
use crate::window_manager::{EventLoopProxy, WindowCommand};
use kurbo::Rect;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Instant,
};
use tokio::sync::{
    mpsc::{UnboundedReceiver, UnboundedSender},
    oneshot,
};

use polymer_composition::{
    ShaderControl, ShellCompositionGroup, ShellRasterChunk, VideoControl, VideoSink, VideoSource,
};

/// Paint output published by Blitz without exposing its document or renderer.
pub(crate) struct ShellFrame {
    pub raster_chunks: Vec<ShellRasterChunk>,
    pub composition_groups: Vec<ShellCompositionGroup>,
    pub surface_layout: Vec<TrackedSurfaceLayout>,
    pub width: u32,
    pub height: u32,
    pub mip_generation: u64,
    pub structure_revision: u64,
}

/// Immutable compositor-facing layout published by the shell DOM.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TrackedSurfaceLayout {
    pub node_id: usize,
    pub source: TrackedSource,
    pub bounds: Rect,
    pub z_order: i32,
    pub group: Option<u64>,
    pub corner_radius: f32,
    pub style: Option<GameViewStyle>,
    pub opacity: f32,
    pub surface_group: Option<SurfaceGroupBounds>,
}

/// The `<tracked-surface-group>` box that encloses a tracked surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SurfaceGroupBounds {
    pub id: u64,
    pub bounds: Rect,
    /// Nearest composition group that contains the whole surface group.
    pub parent: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum TrackedSource {
    Target(u64),
    Video(u64),
}

/// Index of each launch surface that composes into a surface group.
pub(crate) type LaunchSurfaceGroups = std::collections::HashMap<u32, u64>;

/// A pointer position in physical pixels and when the window received it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PointerSample {
    pub x: f64,
    pub y: f64,
    pub at: Instant,
}

/// A window pointer transition. Blitz derives click, over, out, enter, and
/// leave from these.
#[derive(Clone, Copy, Debug)]
pub(crate) enum PointerKind {
    Down,
    Up,
    Move,
}

/// Window input in physical pixels, dispatched to the document in order.
pub(crate) enum ShellInput {
    Pointer {
        kind: PointerKind,
        sample: PointerSample,
        button: u8,
        buttons: u8,
        ctrl_key: bool,
        shift_key: bool,
    },
    Wheel {
        x: f32,
        y: f32,
        horizontal: i32,
        vertical: i32,
        ctrl_key: bool,
        shift_key: bool,
        alt_key: bool,
    },
    Key {
        pressed: bool,
        key: String,
        code: String,
        repeat: bool,
        alt_key: bool,
        ctrl_key: bool,
        shift_key: bool,
    },
    Ime(blitz_traits::events::BlitzImeEvent),
    PointerLeft,
}

impl ShellInput {
    /// The position of a pointer move, which a later move may coalesce.
    pub(crate) fn pointer_move(&self) -> Option<PointerSample> {
        match self {
            Self::Pointer {
                kind: PointerKind::Move,
                sample,
                ..
            } => Some(*sample),
            _ => None,
        }
    }
}

pub(crate) enum ShellCommand {
    /// `sequence` is present when the window thread holds the event until
    /// script decides whether it claims it.
    Input {
        sequence: Option<u64>,
        input: ShellInput,
    },
    Resize {
        width: u32,
        height: u32,
        scale_factor: f32,
    },
    Invalidate,
    /// Replies with each launch surface's group, or the index of the first
    /// launch surface that does not attach to its shell tracked-surface element.
    ValidateLaunch {
        options: serde_json::Value,
        reply: oneshot::Sender<Result<LaunchSurfaceGroups, u32>>,
    },
    Inspect {
        dispatch: Dispatch,
        reply: oneshot::Sender<Result<Reply, ProtocolError>>,
    },
    Media(MediaSignal),
    Wake,
    Shutdown,
}

/// How script handled a held input event.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct InputOutcome {
    pub claimed: bool,
    pub over_tracked_surface: bool,
}

pub(crate) enum ShellEvent {
    Frame(ShellFrame),
    InputSettled {
        sequence: u64,
        outcome: InputOutcome,
    },
    Video {
        id: u64,
        request: VideoRequest,
    },
    Shader {
        target: ShaderTarget,
        control: ShaderControl,
    },
    Capture {
        index: u32,
        reply: oneshot::Sender<SurfaceCapture>,
    },
}

pub(crate) type SurfaceCapture = Result<Option<polymer_runtime::RgbaScreenshot>, String>;

/// The shell element a shader preset is attached to.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ShaderTarget {
    /// A `<tracked-surface-group>`, by its surface-group id.
    Group(u64),
    /// A `<tracked-surface>` presented on its own, by its node id.
    Surface(u64),
}

pub(crate) enum VideoRequest {
    Open {
        source: VideoSource,
        sink: VideoSink,
    },
    Control(VideoControl),
    Extent {
        width: u32,
        height: u32,
    },
    Close,
}

/// Sends commands to the shell thread. Every command wakes it.
#[derive(Clone)]
pub(crate) struct ShellInbox(UnboundedSender<ShellCommand>);

impl ShellInbox {
    pub(super) fn new() -> (Self, UnboundedReceiver<ShellCommand>) {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        (Self(sender), receiver)
    }

    pub(crate) fn send(&self, command: ShellCommand) {
        let _ = self.0.send(command);
    }

    pub(crate) fn wake(&self) {
        self.send(ShellCommand::Wake);
    }
}

/// Delivers shell events to the window thread, posting at most one wake per
/// drain so a burst of frames and input outcomes costs one message.
#[derive(Clone)]
pub(crate) struct ShellOutbox {
    events: mpsc::Sender<ShellEvent>,
    signalled: Arc<AtomicBool>,
    reload: Arc<AtomicBool>,
    proxy: EventLoopProxy<UserEvent>,
}

impl ShellOutbox {
    pub(super) fn new(proxy: EventLoopProxy<UserEvent>) -> (Self, ShellEvents) {
        let (events, received) = mpsc::channel();
        let signalled = Arc::new(AtomicBool::new(false));
        let reload = Arc::new(AtomicBool::new(false));
        let outbox = Self {
            events,
            signalled: signalled.clone(),
            reload: reload.clone(),
            proxy,
        };
        (
            outbox,
            ShellEvents {
                events: received,
                signalled,
                reload,
            },
        )
    }

    pub(crate) fn send(&self, event: ShellEvent) {
        if self.events.send(event).is_ok() {
            self.signal();
        }
    }

    /// Tells the window thread that script asked for the shell to reload.
    pub(super) fn request_reload(&self) {
        self.reload.store(true, Ordering::Release);
        self.signal();
    }

    fn signal(&self) {
        if !self.signalled.swap(true, Ordering::AcqRel) {
            let _ = self
                .proxy
                .send_event(UserEvent::Window(WindowCommand::ShellUpdated));
        }
    }
}

/// The window thread's end of a `ShellOutbox`.
pub(super) struct ShellEvents {
    events: mpsc::Receiver<ShellEvent>,
    signalled: Arc<AtomicBool>,
    reload: Arc<AtomicBool>,
}

impl ShellEvents {
    /// Takes every event published since the last drain, re-arming the wake.
    pub(super) fn drain(&self) -> mpsc::TryIter<'_, ShellEvent> {
        self.signalled.store(false, Ordering::Release);
        self.events.try_iter()
    }

    pub(super) fn reload_requested(&self) -> bool {
        self.reload.load(Ordering::Acquire)
    }
}
