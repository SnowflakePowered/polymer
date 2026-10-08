use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use atomic_write_file::AtomicWriteFile;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Error;
use crate::storage::operation::StorageOperation;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Endpoint {
    pub(super) layer: String,
    pub(super) path: String,
    pub(super) volatile: bool,
}

#[derive(Serialize, Deserialize)]
pub(super) struct RenameRecord {
    pub(super) transaction: Uuid,
    pub(super) session: Uuid,
    pub(super) profile: Uuid,
    pub(super) from: Endpoint,
    pub(super) to: Endpoint,
    pub(super) committed: bool,
    length: u64,
    digest: [u8; 32],
}

pub(super) struct RenameJournal {
    operation: Arc<StorageOperation>,
    profile: Uuid,
    session: Uuid,
}

impl Endpoint {
    pub(super) fn validate(&self) -> Result<(), Error> {
        if self.path.split('/').any(|part| {
            part.is_empty() || matches!(part, "." | ".." | ".git") || part.contains(['\\', ':'])
        }) {
            return Err(Error::InvalidPath(self.path.clone()));
        }
        Ok(())
    }
}

impl RenameJournal {
    pub(super) fn new(operation: Arc<StorageOperation>, profile: Uuid) -> Self {
        Self {
            operation,
            profile,
            session: Uuid::new_v4(),
        }
    }

    fn payload_path(&self, transaction: Uuid) -> PathBuf {
        self.operation
            .pending
            .with_file_name(format!("{transaction}.payload"))
    }

    pub(super) fn resumed(&self, record: &RenameRecord) -> bool {
        record.session != self.session
    }

    pub(super) fn load(&self) -> Result<Option<RenameRecord>, Error> {
        let pending = &self.operation.pending;
        let input = match File::open(pending) {
            Ok(input) => input,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.operation.recovering.store(false, Ordering::Release);
                return Ok(None);
            }
            Err(error) => return Err(Error::io(pending, error)),
        };
        self.operation.recovering.store(true, Ordering::Release);
        let record: RenameRecord =
            serde_json::from_reader(input.take(64 * 1024)).map_err(|error| {
                Error::HistoryConflict(format!("invalid composed rename journal: {error}"))
            })?;
        record.from.validate()?;
        record.to.validate()?;
        if record.profile != self.profile || record.from.path == record.to.path {
            return Err(Error::HistoryConflict(
                "composed rename journal identity mismatch".to_owned(),
            ));
        }
        Ok(Some(record))
    }

    fn store(&self, record: &RenameRecord) -> Result<(), Error> {
        let bytes = serde_json::to_vec(record).map_err(|error| {
            Error::HistoryConflict(format!("encode composed rename journal: {error}"))
        })?;
        let pending = &self.operation.pending;
        self.operation.recovering.store(true, Ordering::Release);
        let mut file = AtomicWriteFile::open(pending).map_err(|error| Error::io(pending, error))?;
        file.write_all(&bytes)
            .map_err(|error| Error::io(pending, error))?;
        file.as_file()
            .sync_all()
            .map_err(|error| Error::io(pending, error))?;
        file.commit().map_err(|error| Error::io(pending, error))
    }

    pub(super) fn prepare(
        &self,
        from: Endpoint,
        to: Endpoint,
        source: &mut dyn Read,
    ) -> Result<RenameRecord, Error> {
        let directory = self
            .operation
            .pending
            .parent()
            .expect("operation marker has a directory");
        fs::create_dir_all(directory).map_err(|error| Error::io(directory, error))?;
        let mut record = RenameRecord {
            transaction: Uuid::new_v4(),
            session: self.session,
            profile: self.profile,
            from,
            to,
            committed: false,
            length: 0,
            digest: [0; 32],
        };
        let path = self.payload_path(record.transaction);
        let committed = (|| {
            self.store(&record)?;
            let mut payload = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
                .map_err(|error| Error::io(&path, error))?;
            let mut buffer = vec![0; 64 * 1024];
            let mut hasher = blake3::Hasher::new();
            loop {
                let read = source
                    .read(&mut buffer)
                    .map_err(|error| Error::io(&path, error))?;
                if read == 0 {
                    break;
                }
                payload
                    .write_all(&buffer[..read])
                    .map_err(|error| Error::io(&path, error))?;
                hasher.update(&buffer[..read]);
                record.length += read as u64;
            }
            payload
                .sync_all()
                .map_err(|error| Error::io(&path, error))?;
            record.digest = *hasher.finalize().as_bytes();
            record.committed = true;
            self.store(&record)
        })();
        if committed.is_err() {
            let _ = self.clear(&record);
        }
        committed.map(|()| record)
    }

    pub(super) fn payload(&self, record: &RenameRecord) -> Result<File, Error> {
        let path = self.payload_path(record.transaction);
        let mut payload = File::open(&path).map_err(|error| Error::io(&path, error))?;
        let mut hasher = blake3::Hasher::new();
        let length =
            std::io::copy(&mut payload, &mut hasher).map_err(|error| Error::io(&path, error))?;
        if length != record.length || hasher.finalize().as_bytes() != &record.digest {
            return Err(Error::HistoryConflict(
                "composed rename payload is corrupt".to_owned(),
            ));
        }
        payload
            .seek(SeekFrom::Start(0))
            .map_err(|error| Error::io(&path, error))?;
        Ok(payload)
    }

    pub(super) fn clear(&self, record: &RenameRecord) -> Result<(), Error> {
        let pending = &self.operation.pending;
        match fs::remove_file(pending) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(Error::io(pending, error)),
        }
        self.operation.recovering.store(false, Ordering::Release);
        let _ = fs::remove_file(self.payload_path(record.transaction));
        Ok(())
    }

    pub(super) fn collect_payloads(&self) -> Result<(), Error> {
        let directory = self
            .operation
            .pending
            .parent()
            .expect("operation marker has a directory");
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(Error::io(directory, error)),
        };
        for entry in entries {
            let path = entry.map_err(|error| Error::io(directory, error))?.path();
            if path.extension() == Some(std::ffi::OsStr::new("payload"))
                && path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| Uuid::parse_str(name).is_ok())
            {
                fs::remove_file(&path).map_err(|error| Error::io(&path, error))?;
            }
        }
        Ok(())
    }
}
