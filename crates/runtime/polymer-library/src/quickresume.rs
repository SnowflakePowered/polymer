use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

const METADATA_FILE: &str = "metadata.json";
const STATE_FILE: &str = "state.bin";
const SCREENSHOT_FILE: &str = "screenshot.png";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuickResumeKey(String);

impl QuickResumeKey {
    pub fn new(emulator: &str, platform: &str, identity: &str) -> Self {
        let mut hash = blake3::Hasher::new();
        for value in [emulator, platform, identity] {
            hash.update(&(value.len() as u64).to_le_bytes());
            hash.update(value.as_bytes());
        }
        Self(hash.finalize().to_hex().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn from_storage_key(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then_some(Self(value))
    }
}

#[derive(Clone, Debug)]
pub struct QuickResumeEntry {
    pub state: Vec<u8>,
    pub screenshot: Option<Vec<u8>>,
    pub disc_index: u32,
}

#[derive(Clone, Debug)]
pub struct QuickResumeStore {
    root: PathBuf,
}

#[derive(Deserialize, Serialize)]
struct Metadata {
    revision: String,
    screenshot: bool,
    #[serde(default)]
    disc_index: u32,
}

impl QuickResumeStore {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_owned(),
        }
    }

    pub fn discover() -> io::Result<Self> {
        let directories =
            crate::Directories::discover().map_err(|error| io::Error::other(error.to_string()))?;
        Ok(Self::new(directories.quick_resume()))
    }

    pub fn take(
        &self,
        key: &QuickResumeKey,
        revision: &str,
    ) -> io::Result<Option<QuickResumeEntry>> {
        let directory = self.root.join(key.as_str());
        if !directory.exists() {
            return Ok(None);
        }
        let loaded = (|| {
            let metadata: Metadata =
                serde_json::from_slice(&fs::read(directory.join(METADATA_FILE))?)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            if metadata.revision != revision {
                return Ok(None);
            }
            let state = fs::read(directory.join(STATE_FILE))?;
            let screenshot = if metadata.screenshot {
                Some(fs::read(directory.join(SCREENSHOT_FILE))?)
            } else {
                None
            };
            Ok(Some(QuickResumeEntry {
                state,
                screenshot,
                disc_index: metadata.disc_index,
            }))
        })();
        if let Err(error) = fs::remove_dir_all(&directory)
            && error.kind() != io::ErrorKind::NotFound
        {
            return Err(error);
        }
        loaded
    }

    pub fn commit(
        &self,
        key: &QuickResumeKey,
        revision: &str,
        state: &[u8],
        screenshot: Option<&[u8]>,
        disc_index: u32,
    ) -> io::Result<()> {
        fs::create_dir_all(&self.root)?;
        let target = self.root.join(key.as_str());
        let staging = self
            .root
            .join(format!(".{}.{}", key.as_str(), uuid::Uuid::new_v4()));
        fs::create_dir(&staging)?;
        let result = (|| {
            fs::write(staging.join(STATE_FILE), state)?;
            let screenshot = screenshot.is_some_and(|screenshot| {
                match fs::write(staging.join(SCREENSHOT_FILE), screenshot) {
                    Ok(()) => true,
                    Err(error) => {
                        tracing::warn!(
                            "polymer: Quick Resume screenshot persistence failed: {error}"
                        );
                        let _ = fs::remove_file(staging.join(SCREENSHOT_FILE));
                        false
                    }
                }
            });
            fs::write(
                staging.join(METADATA_FILE),
                serde_json::to_vec(&Metadata {
                    revision: revision.to_owned(),
                    screenshot,
                    disc_index,
                })?,
            )?;
            if target.exists() {
                fs::remove_dir_all(&target)?;
            }
            fs::rename(&staging, &target)
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&staging);
        }
        result
    }

    pub fn remove(&self, key: &QuickResumeKey) -> io::Result<()> {
        match fs::remove_dir_all(self.root.join(key.as_str())) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}
