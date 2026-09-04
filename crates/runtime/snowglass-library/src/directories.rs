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

        Ok(Self::rooted(base.cache_dir(), base.data_dir()))
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
        self.cache.join("plugins").join(plugin)
    }

    pub fn jsr_cache(&self) -> PathBuf {
        self.cache.join("jsr")
    }

    pub fn plugin_scratch(&self, plugin: &str) -> PathBuf {
        self.cache.join("scratch").join(plugin)
    }

    pub fn quick_resume(&self) -> PathBuf {
        self.cache.join("quick-resume")
    }

    pub fn consolestorage(&self) -> PathBuf {
        self.storage.clone()
    }

    pub fn webstorage(&self) -> PathBuf {
        self.storage.join("webstorage.db")
    }

    pub fn library_database(&self) -> PathBuf {
        self.storage.join("library.db")
    }

    pub fn plugin_database(&self, plugin: &str) -> PathBuf {
        self.storage.join("plugins").join(plugin).join("plugin.db")
    }

    pub fn object_storage(&self) -> PathBuf {
        self.storage.join("objects")
    }
}
