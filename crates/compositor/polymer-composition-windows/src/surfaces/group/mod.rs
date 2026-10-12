mod session;

use polymer_composition::{ShaderContent, ShaderControl, SurfaceFrameEvent, SurfaceGroupLayout};
use polymer_renderer_d3d12::D3d12Compositor;
use polymer_runtime::{SurfaceFrameReceiver, SurfaceFrameUpdate};
use session::GroupSession;
use std::{
    collections::HashMap,
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
use windows::core::IUnknown;

const IDLE_WAIT: Duration = Duration::from_millis(100);
const REPLY_WAIT: Duration = Duration::from_secs(2);

pub(super) type Import = Box<dyn FnOnce(&mut D3d12Compositor) -> Result<(), Box<dyn Error>> + Send>;
pub(super) type SnapshotResult = Result<Option<polymer_runtime::RgbaScreenshot>, String>;

/// Where presentation threads report back to the window thread.
#[derive(Clone)]
pub(super) struct GroupEvents {
    pub(super) frames: mpsc::Sender<SurfaceFrameEvent>,
    pub(super) shaders: mpsc::Sender<ShaderContent>,
}

enum GroupMessage {
    Import {
        connection: u64,
        surface_id: u64,
        import: Import,
        reply: mpsc::SyncSender<Result<(), String>>,
    },
    Frame(u64, SurfaceFrameUpdate),
    Remove(u64),
    Arrange(SurfaceGroupLayout),
    Shader(ShaderControl),
    Snapshot(u64, mpsc::SyncSender<SnapshotResult>),
    Stop,
}

/// Presents captured streams into one swapchain on its own thread: every
/// stream of a `<tracked-surface-group>`, or one stream presented on its own.
pub(super) struct SurfaceGroup {
    content: IUnknown,
    messages: mpsc::Sender<GroupMessage>,
    members: HashMap<u64, Arc<AtomicBool>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl SurfaceGroup {
    /// Starts presenting `compositor`, whose already imported streams are `imported`.
    pub(super) fn spawn(
        id: u64,
        compositor: D3d12Compositor,
        events: GroupEvents,
        imported: &[(u64, u64)],
    ) -> Result<Self, Box<dyn Error>> {
        let content = compositor.composition_content()?;
        let (messages, receiver) = mpsc::channel();
        let surfaces = imported.iter().copied().collect();
        let worker = thread::Builder::new()
            .name(format!("polymer-surface-group-{id}"))
            .spawn(move || {
                if let Err(error) =
                    GroupSession::new(id, compositor, events, surfaces).run(&receiver)
                {
                    tracing::error!("polymer: presenting surface group {id} failed: {error}");
                }
            })?;
        Ok(Self {
            content,
            messages,
            members: imported
                .iter()
                .map(|&(connection, _)| (connection, Arc::new(AtomicBool::new(false))))
                .collect(),
            worker: Some(worker),
        })
    }

    pub(super) fn content(&self) -> IUnknown {
        self.content.clone()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    pub(super) fn import(
        &mut self,
        connection: u64,
        surface_id: u64,
        import: Import,
    ) -> Result<(), Box<dyn Error>> {
        let (reply, response) = mpsc::sync_channel(1);
        self.messages
            .send(GroupMessage::Import {
                connection,
                surface_id,
                import,
                reply,
            })
            .map_err(|_| "surface group worker stopped")?;
        self.members
            .insert(connection, Arc::new(AtomicBool::new(false)));
        let imported = response
            .recv_timeout(REPLY_WAIT)
            .map_err(|_| "surface group import timed out".to_owned())
            .and_then(|imported| imported);
        if imported.is_err() {
            self.remove(connection);
        }
        Ok(imported?)
    }

    /// Forwards one member's frames into the group's single wake-up channel.
    pub(super) fn attach(
        &self,
        connection: u64,
        mut frames: SurfaceFrameReceiver,
    ) -> Result<(), Box<dyn Error>> {
        let stopping = Arc::clone(
            self.members
                .get(&connection)
                .ok_or("surface is not a member of this group")?,
        );
        let messages = self.messages.clone();
        thread::Builder::new()
            .name(format!("polymer-surface-{connection}"))
            .spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    match frames.next_frame_timeout(IDLE_WAIT) {
                        Ok(Some(update)) => {
                            if messages
                                .send(GroupMessage::Frame(connection, update))
                                .is_err()
                            {
                                break;
                            }
                        }
                        Ok(None) => {}
                        Err(_) => break,
                    }
                }
            })?;
        Ok(())
    }

    pub(super) fn remove(&mut self, connection: u64) {
        if let Some(stopping) = self.members.remove(&connection) {
            stopping.store(true, Ordering::Release);
            let _ = self.messages.send(GroupMessage::Remove(connection));
        }
    }

    pub(super) fn arrange(&self, layout: SurfaceGroupLayout) {
        let _ = self.messages.send(GroupMessage::Arrange(layout));
    }

    pub(super) fn control_shader(&self, control: ShaderControl) {
        let _ = self.messages.send(GroupMessage::Shader(control));
    }

    pub(super) fn snapshot(&self, connection: u64) -> SnapshotResult {
        let (reply, response) = mpsc::sync_channel(1);
        self.messages
            .send(GroupMessage::Snapshot(connection, reply))
            .map_err(|_| "surface group worker stopped".to_owned())?;
        response
            .recv_timeout(REPLY_WAIT)
            .map_err(|_| "surface screenshot timed out".to_owned())?
    }
}

impl Drop for SurfaceGroup {
    fn drop(&mut self) {
        for stopping in self.members.values() {
            stopping.store(true, Ordering::Release);
        }
        let _ = self.messages.send(GroupMessage::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
