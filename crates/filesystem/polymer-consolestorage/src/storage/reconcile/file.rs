use super::worker::ReconcileWorker;
use crate::{Error, Metadata, StorageFile};
use std::sync::Arc;

pub(super) struct ReconcileFile {
    pub(super) file: Arc<dyn StorageFile>,
    pub(super) worker: ReconcileWorker,
    pub(super) schedule_writeback: bool,
}

impl StorageFile for ReconcileFile {
    fn metadata(&self) -> Result<Metadata, Error> {
        self.file.metadata()
    }
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<usize, Error> {
        self.file.read_at(offset, buffer)
    }

    fn write_at(&self, offset: u64, buffer: &[u8]) -> Result<usize, Error> {
        let mutation = self.worker.begin(self.schedule_writeback)?;
        let written = self.file.write_at(offset, buffer)?;
        mutation.complete()?;
        Ok(written)
    }

    fn set_len(&self, len: u64) -> Result<(), Error> {
        let mutation = self.worker.begin(self.schedule_writeback)?;
        self.file.set_len(len)?;
        mutation.complete()?;
        Ok(())
    }

    fn flush(&self) -> Result<(), Error> {
        let mutation = self.worker.begin(self.schedule_writeback)?;
        self.file.flush()?;
        mutation.complete()?;
        Ok(())
    }

    fn sync(&self) -> Result<(), Error> {
        self.file.sync()?;
        self.worker.reconcile()
    }
}
