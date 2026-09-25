use super::DepotType;
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::Arc;

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

/// Reads through `inner`, reporting each read as completed bytes.
pub(crate) struct ReportingReader<'a, R> {
    pub inner: R,
    pub report: &'a dyn Fn(InstallEvent),
}

impl<R: Read> Read for ReportingReader<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(bytes)?;
        (self.report)(InstallEvent::Advance(count as u64));
        Ok(count)
    }
}
