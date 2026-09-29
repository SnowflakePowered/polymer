use std::fmt;
use std::sync::Arc;

use crate::savegame::SaveDepotId;
use crate::{EntryKind, Error, MountableStorage, Storage};

pub type Reconcile = Arc<dyn Fn(&[u8]) -> Result<(), Error> + Send + Sync>;

#[derive(Clone)]
pub struct MemoryCardView {
    storage: Arc<dyn MountableStorage>,
    entry: String,
    kind: EntryKind,
}

impl MemoryCardView {
    pub fn new(
        storage: Arc<dyn MountableStorage>,
        entry: impl Into<String>,
    ) -> Result<Self, Error> {
        let entry = entry.into();
        if !entry.is_empty()
            && entry.split('/').any(|component| {
                component.is_empty()
                    || component == "."
                    || component == ".."
                    || component.contains('\\')
            })
        {
            return Err(Error::InvalidPath(entry));
        }
        let kind = storage.metadata(&entry)?.kind;
        Ok(Self {
            storage,
            entry,
            kind,
        })
    }

    pub fn storage(&self) -> &Arc<dyn MountableStorage> {
        &self.storage
    }

    pub fn entry(&self) -> &str {
        &self.entry
    }

    pub const fn kind(&self) -> EntryKind {
        self.kind
    }
}

impl fmt::Debug for MemoryCardView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MemoryCardView")
            .field("storage", &self.storage)
            .field("entry", &self.entry)
            .field("kind", &self.kind)
            .finish()
    }
}

pub trait MemoryCard: fmt::Debug + Send + Sync {
    const RAW_RECONCILE_NAME: &'static str;

    fn storage(&self) -> &dyn Storage;

    fn view(&self, primary: &[SaveDepotId], format: &str) -> Result<MemoryCardView, Error>;

    fn reconcile(&self, primary: &[SaveDepotId]) -> Result<Reconcile, Error>;

    fn raw_image(&self, primary: &[SaveDepotId]) -> Result<Vec<u8>, Error>;

    fn raw_view(&self, primary: &[SaveDepotId]) -> Result<MemoryCardView, Error>
    where
        Self: Clone + Sized + 'static,
    {
        let image = self.raw_image(primary)?;
        let storage =
            crate::impls::memorycard::MemoryCardStorage::new(self.clone(), primary, image)?;
        MemoryCardView::new(
            Arc::new(crate::ReconcileStorage::new(
                storage,
                Self::RAW_RECONCILE_NAME,
            )?),
            "",
        )
    }
}
