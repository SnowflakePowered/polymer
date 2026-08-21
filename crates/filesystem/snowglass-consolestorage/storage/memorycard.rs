use std::fmt;
use std::sync::Arc;

use crate::savegame::SaveDepotId;
use crate::{EntryKind, Error, MountableStorage, Storage};

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

pub trait MemoryCard: Storage {
    fn block_size(&self) -> u64;

    fn max_blocks(&self) -> u32;

    fn view(
        &self,
        primary: &[SaveDepotId],
        capacity_blocks: u32,
        format: &str,
    ) -> Result<MemoryCardView, Error>;
}
