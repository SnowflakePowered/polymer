mod bindings;
pub(crate) use bindings::extension;

use polymer_library::firmware::{FirmwareError, FirmwareInstallDescriptor, FirmwareStore};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::OnceCell;

#[derive(Clone)]
pub(crate) struct FirmwareBinding {
    pub root: PathBuf,
    pub platform: Option<String>,
    pub input: Option<(String, Arc<[u8]>)>,
    store: Arc<OnceCell<FirmwareStore>>,
    pub results: Arc<parking_lot::Mutex<Vec<FirmwareInstallDescriptor>>>,
}

impl FirmwareBinding {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            platform: None,
            input: None,
            store: Default::default(),
            results: Default::default(),
        }
    }

    pub async fn store(&self) -> Result<&FirmwareStore, FirmwareError> {
        self.store
            .get_or_try_init(|| FirmwareStore::open(&self.root))
            .await
    }
}
