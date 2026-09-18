mod bindings;
pub(crate) use bindings::extension;

use polymer_consolestorage::keys::KeyError;
use polymer_library::keys::KeyStore;
use std::{path::PathBuf, sync::Arc};
use tokio::sync::OnceCell;

#[derive(Clone)]
pub(crate) struct KeyBinding {
    pub(crate) root: PathBuf,
    pub platform: Option<String>,
    pub allow_write: bool,
    store: Arc<OnceCell<KeyStore>>,
}

impl KeyBinding {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            platform: None,
            allow_write: false,
            store: Default::default(),
        }
    }

    async fn platform_keys(&self) -> Result<polymer_library::keys::PlatformKeys, KeyError> {
        let platform = self
            .platform
            .as_deref()
            .ok_or_else(|| KeyError::Invalid("keyStore requires a host-bound platform".into()))?;
        self.store
            .get_or_try_init(|| KeyStore::open(&self.root))
            .await?
            .for_platform(platform)
    }
}
