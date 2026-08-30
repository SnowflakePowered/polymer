use super::{MAX_STORAGE_BYTES, WebStorageError};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub(super) struct SessionStorage {
    entries: Arc<Mutex<Vec<(String, String)>>>,
}

impl SessionStorage {
    fn entries(&self) -> Result<std::sync::MutexGuard<'_, Vec<(String, String)>>, WebStorageError> {
        self.entries
            .lock()
            .map_err(|_| WebStorageError::StateUnavailable)
    }

    pub(super) fn length(&self) -> Result<u32, WebStorageError> {
        u32::try_from(self.entries()?.len()).map_err(|_| WebStorageError::StateUnavailable)
    }

    pub(super) fn key(&self, index: u32) -> Result<Option<String>, WebStorageError> {
        Ok(self
            .entries()?
            .get(index as usize)
            .map(|(key, _)| key.clone()))
    }

    pub(super) fn get_item(&self, key: &str) -> Result<Option<String>, WebStorageError> {
        Ok(self
            .entries()?
            .iter()
            .find(|(stored, _)| stored == key)
            .map(|(_, value)| value.clone()))
    }

    pub(super) fn set_item(&self, key: String, value: String) -> Result<(), WebStorageError> {
        let mut entries = self.entries()?;
        let replaced_size = entries
            .iter()
            .find(|(stored, _)| stored == &key)
            .map_or(0, |(key, value)| key.len() + value.len());
        let current_size = entries
            .iter()
            .map(|(key, value)| key.len() + value.len())
            .sum::<usize>();
        let new_size = current_size
            .checked_sub(replaced_size)
            .and_then(|size| size.checked_add(key.len() + value.len()))
            .ok_or(WebStorageError::StorageExceeded)?;
        if new_size >= MAX_STORAGE_BYTES {
            return Err(WebStorageError::StorageExceeded);
        }
        match entries.iter_mut().find(|(stored, _)| stored == &key) {
            Some((_, stored)) => *stored = value,
            None => entries.push((key, value)),
        }
        Ok(())
    }

    pub(super) fn remove_item(&self, key: &str) -> Result<(), WebStorageError> {
        self.entries()?.retain(|(stored, _)| stored != key);
        Ok(())
    }

    pub(super) fn clear(&self) -> Result<(), WebStorageError> {
        self.entries()?.clear();
        Ok(())
    }
}
