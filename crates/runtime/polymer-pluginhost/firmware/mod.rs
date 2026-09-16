mod bindings;
pub(crate) use bindings::extension;

use polymer_library::firmware::{FirmwareError, FirmwareStore};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::OnceCell;

#[derive(Clone)]
pub(crate) struct FirmwareBinding {
    pub root: PathBuf,
    pub platform: Option<String>,
    store: Arc<OnceCell<FirmwareStore>>,
}

impl FirmwareBinding {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            platform: None,
            store: Default::default(),
        }
    }

    pub async fn store(&self) -> Result<&FirmwareStore, FirmwareError> {
        self.store
            .get_or_try_init(|| FirmwareStore::open(&self.root))
            .await
    }
}
