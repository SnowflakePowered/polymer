use crate::StorageFile;
use std::io::{self, Write};
use std::sync::Arc;

pub(crate) struct StorageWriter {
    pub file: Arc<dyn StorageFile>,
    pub position: u64,
    pub hash: blake3::Hasher,
}

impl Write for StorageWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.file
            .write_all_at(self.position, bytes)
            .map_err(io::Error::other)?;
        self.position += bytes.len() as u64;
        self.hash.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush().map_err(io::Error::other)
    }
}
