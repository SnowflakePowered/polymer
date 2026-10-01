use super::DepotType;
use genawaiter::rc::Co;
use std::fmt;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::pin::{Pin, pin};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, ReadBuf};
use tokio::sync::watch;

/// What an installation will produce, known before any contents are extracted.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallPreview {
    pub id: String,
    pub label: String,
    pub depot_type: DepotType,
    pub icon: Option<String>,
}

/// Progress yielded while an installation runs.
#[derive(Clone, Debug)]
pub enum InstallEvent {
    /// The title being installed, yielded first when the depot can describe it.
    Title(InstallPreview),
    /// A phase over `total` bytes began; a zero total is indeterminate.
    Phase { phase: InstallPhase, total: u64 },
    /// Bytes completed in the current phase.
    Advance(u64),
    /// A file was extracted into the NAND.
    File(PathBuf),
}

/// A stage of an installation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum InstallPhase {
    #[default]
    Preparing,
    Verifying,
    Installing,
    Finalizing,
}

impl fmt::Display for InstallPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Preparing => "Preparing",
            Self::Verifying => "Verifying",
            Self::Installing => "Installing",
            Self::Finalizing => "Finalizing",
        })
    }
}

/// The latest progress of an installation. `generation` counts phases, so a phase
/// that repeats the previous phase and total is still reported.
#[derive(Clone, Copy, Default)]
pub(super) struct Snapshot {
    generation: u64,
    phase: InstallPhase,
    total: u64,
    completed: u64,
}

/// Reports an installation's progress. Installers start blocking work with
/// [`InstallContext::spawn_blocking`], which hands the work its own context.
/// The default context reports nowhere.
#[derive(Clone)]
pub struct InstallContext {
    progress: watch::Sender<Snapshot>,
}

impl Default for InstallContext {
    fn default() -> Self {
        Self {
            progress: watch::Sender::new(Snapshot::default()),
        }
    }
}

impl InstallContext {
    pub(super) fn new() -> (Self, watch::Receiver<Snapshot>) {
        let (progress, changes) = watch::channel(Snapshot::default());
        (Self { progress }, changes)
    }

    /// Begin `phase` over `total` bytes; a zero total is indeterminate.
    pub fn phase(&self, phase: InstallPhase, total: u64) {
        self.progress.send_modify(|state| {
            *state = Snapshot {
                generation: state.generation + 1,
                phase,
                total,
                completed: 0,
            }
        });
    }

    pub fn advance(&self, bytes: u64) {
        self.progress.send_modify(|state| state.completed += bytes);
    }

    /// Run blocking work on a worker thread, reporting through the context it is given.
    pub fn spawn_blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce(&InstallContext) -> T + Send + 'static,
    ) -> tokio::task::JoinHandle<T> {
        let context = self.clone();
        tokio::task::spawn_blocking(move || work(&context))
    }
}

/// Awaits an installation's work, yielding each change to its progress to the
/// installation's generator as soon as it is reported.
pub(super) struct ProgressReporter {
    co: Co<InstallEvent>,
    changes: watch::Receiver<Snapshot>,
    seen: Snapshot,
}

impl ProgressReporter {
    pub(super) fn new(co: Co<InstallEvent>, changes: watch::Receiver<Snapshot>) -> Self {
        Self {
            co,
            changes,
            seen: Snapshot::default(),
        }
    }

    pub(super) async fn emit(&self, event: InstallEvent) {
        self.co.yield_(event).await;
    }

    /// Await `work`, yielding its progress whenever it changes. Progress reported
    /// while an event is being consumed is combined into the next event.
    pub(super) async fn run<T>(&mut self, work: impl Future<Output = T>) -> T {
        let mut work = pin!(work);
        loop {
            tokio::select! {
                biased;
                output = &mut work => {
                    self.flush().await;
                    return output;
                }
                Ok(()) = self.changes.changed() => self.flush().await,
            }
        }
    }

    async fn flush(&mut self) {
        let state = *self.changes.borrow_and_update();
        if state.generation != self.seen.generation {
            self.emit(InstallEvent::Phase {
                phase: state.phase,
                total: state.total,
            })
            .await;
            self.seen = Snapshot {
                completed: 0,
                ..state
            };
        }
        if state.completed > self.seen.completed {
            self.emit(InstallEvent::Advance(state.completed - self.seen.completed))
                .await;
            self.seen.completed = state.completed;
        }
    }
}

/// Passes reads and writes through to `inner`, reporting each transfer as completed bytes.
pub(crate) struct Reporting<T> {
    pub inner: T,
    pub context: InstallContext,
}

impl<R: Read> Read for Reporting<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(bytes)?;
        self.context.advance(count as u64);
        Ok(count)
    }
}

impl<W: Write> Write for Reporting<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = self.inner.write(bytes)?;
        self.context.advance(count as u64);
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for Reporting<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buffer.filled().len();
        let poll = Pin::new(&mut self.inner).poll_read(cx, buffer);
        if let Poll::Ready(Ok(())) = poll {
            self.context
                .advance((buffer.filled().len() - before) as u64);
        }
        poll
    }
}
