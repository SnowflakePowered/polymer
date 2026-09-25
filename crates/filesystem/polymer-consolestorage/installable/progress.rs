use super::DepotType;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, ReadBuf};

/// What an installation will produce, known before any contents are extracted.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallPreview {
    pub id: String,
    pub label: String,
    pub depot_type: DepotType,
    pub icon: Option<String>,
}

/// Progress published while an installation runs.
#[derive(Clone, Debug)]
pub enum InstallEvent {
    /// The title being installed, published first when the depot can describe it.
    Title(InstallPreview),
    /// A phase over `total` bytes began; a zero total is indeterminate.
    Phase { name: String, total: u64 },
    /// Bytes completed in the current phase.
    Advance(u64),
    /// A file was extracted into the NAND.
    File(PathBuf),
}

/// Receives an installation's progress, including from extraction workers.
pub type InstallEvents = Arc<dyn Fn(InstallEvent) + Send + Sync>;

/// Passes reads and writes through to `inner`, publishing each transfer as completed bytes.
pub(crate) struct Reporting<T> {
    pub inner: T,
    pub events: InstallEvents,
}

impl<R: Read> Read for Reporting<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(bytes)?;
        (self.events)(InstallEvent::Advance(count as u64));
        Ok(count)
    }
}

impl<W: Write> Write for Reporting<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = self.inner.write(bytes)?;
        (self.events)(InstallEvent::Advance(count as u64));
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
            (self.events)(InstallEvent::Advance(
                (buffer.filled().len() - before) as u64,
            ));
        }
        poll
    }
}
