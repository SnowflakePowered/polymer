use std::path::{Path, PathBuf};

use directories::BaseDirs;
use snowglass_consolestorage::Error;

#[derive(Debug, Clone)]
pub struct Directories {
    cache: PathBuf,
    storage: PathBuf,
}

impl Directories {
    pub fn discover() -> Result<Self, Error> {
        let base = BaseDirs::new()
            .ok_or_else(|| Error::Unsupported("no home directory for this user".to_string()))?;

        Ok(Self::rooted(base.cache_dir(), base.data_local_dir()))
    }

    pub fn rooted(cache: impl AsRef<Path>, storage: impl AsRef<Path>) -> Self {
        Self {
            cache: cache.as_ref().join(".snowglass").join("cache"),
            storage: storage.as_ref().join(".snowglass").join("storage"),
        }
    }

    pub fn cache(&self) -> &Path {
        &self.cache
    }

    pub fn storage(&self) -> &Path {
        &self.storage
    }

    pub fn plugin_cache(&self, plugin: &str) -> PathBuf {
        self.cache.join(plugin)
    }

    pub fn consolestorage(&self) -> PathBuf {
        self.storage.join("consolestorage")
    }
}
