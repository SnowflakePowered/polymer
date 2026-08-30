use deno_core::{Extension, OpState, op2};
use snowglass_library::Directories;
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, Mutex},
};

const MAX_STORAGE_BYTES: usize = 10 * 1024 * 1024;
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS webstorage (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

PRAGMA user_version = 1;
"#;

#[derive(Debug, thiserror::Error, deno_error::JsError)]
pub enum WebStorageError {
    #[class("DOMExceptionNotSupportedError")]
    #[error("localStorage is not supported in this context")]
    ContextNotSupported,
    #[class("DOMExceptionQuotaExceededError")]
    #[error("exceeded maximum storage size")]
    StorageExceeded,
    #[class(generic)]
    #[error("could not create the webstorage database directory: {0}")]
    CreateDirectory(#[source] std::io::Error),
    #[class(generic)]
    #[error("the webstorage database path is not valid UTF-8")]
    InvalidPath,
    #[class(generic)]
    #[error("webstorage database error: {0}")]
    Database(#[from] turso::Error),
    #[class(generic)]
    #[error("webstorage database schema version {0} is not supported")]
    UnsupportedSchema(i64),
    #[class(generic)]
    #[error("webstorage state is unavailable")]
    StateUnavailable,
}

#[derive(Clone)]
pub struct LocalStorage {
    connection: turso::Connection,
}

impl LocalStorage {
    pub async fn open(directories: &Directories) -> Result<Self, WebStorageError> {
        let path = directories.webstorage();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(WebStorageError::CreateDirectory)?;
        }
        let path = path.to_str().ok_or(WebStorageError::InvalidPath)?;
        let database = turso::Builder::new_local(path).build().await?;
        let connection = database.connect()?;
        let mut rows = connection.query("PRAGMA user_version", ()).await?;
        let version = rows
            .next()
            .await?
            .ok_or(WebStorageError::StateUnavailable)?
            .get::<i64>(0)?;
        drop(rows);
        match version {
            0 => connection.execute_batch(SCHEMA).await?,
            1 => {}
            version => return Err(WebStorageError::UnsupportedSchema(version)),
        }
        Ok(Self { connection })
    }

    pub async fn length(&self) -> Result<u32, WebStorageError> {
        let mut rows = self
            .connection
            .query("SELECT COUNT(*) FROM webstorage", ())
            .await?;
        let length = rows
            .next()
            .await?
            .ok_or(WebStorageError::StateUnavailable)?
            .get::<i64>(0)?;
        u32::try_from(length).map_err(|_| WebStorageError::StateUnavailable)
    }

    pub async fn key(&self, index: u32) -> Result<Option<String>, WebStorageError> {
        let mut rows = self
            .connection
            .query(
                "SELECT key FROM webstorage ORDER BY rowid LIMIT 1 OFFSET ?1",
                [i64::from(index)],
            )
            .await?;
        rows.next()
            .await?
            .map(|row| row.get::<String>(0).map_err(WebStorageError::from))
            .transpose()
    }

    pub async fn get_item(&self, key: &str) -> Result<Option<String>, WebStorageError> {
        let mut rows = self
            .connection
            .query("SELECT value FROM webstorage WHERE key = ?1", [key])
            .await?;
        rows.next()
            .await?
            .map(|row| row.get::<String>(0).map_err(WebStorageError::from))
            .transpose()
    }

    pub async fn set_item(&self, key: &str, value: &str) -> Result<(), WebStorageError> {
        if key.len() + value.len() >= MAX_STORAGE_BYTES {
            return Err(WebStorageError::StorageExceeded);
        }
        let mut rows = self
            .connection
            .query(
                r#"
                SELECT
                    COALESCE(SUM(length(CAST(key AS BLOB)) + length(CAST(value AS BLOB))), 0)
                    - COALESCE((
                        SELECT length(CAST(key AS BLOB)) + length(CAST(value AS BLOB))
                        FROM webstorage
                        WHERE key = ?1
                    ), 0)
                FROM webstorage
                "#,
                [key],
            )
            .await?;
        let current_size = rows
            .next()
            .await?
            .ok_or(WebStorageError::StateUnavailable)?
            .get::<i64>(0)?;
        drop(rows);
        let new_size = usize::try_from(current_size)
            .ok()
            .and_then(|size| size.checked_add(key.len() + value.len()))
            .ok_or(WebStorageError::StorageExceeded)?;
        if new_size >= MAX_STORAGE_BYTES {
            return Err(WebStorageError::StorageExceeded);
        }
        self.connection
            .execute(
                r#"
                INSERT INTO webstorage (key, value) VALUES (?1, ?2)
                ON CONFLICT (key) DO UPDATE SET value = excluded.value
                "#,
                (key, value),
            )
            .await?;
        Ok(())
    }

    pub async fn remove_item(&self, key: &str) -> Result<(), WebStorageError> {
        self.connection
            .execute("DELETE FROM webstorage WHERE key = ?1", [key])
            .await?;
        Ok(())
    }

    pub async fn clear(&self) -> Result<(), WebStorageError> {
        self.connection
            .execute("DELETE FROM webstorage", ())
            .await?;
        Ok(())
    }
}

#[derive(Clone, Default)]
struct SessionStorage {
    entries: Arc<Mutex<Vec<(String, String)>>>,
}

impl SessionStorage {
    fn entries(&self) -> Result<std::sync::MutexGuard<'_, Vec<(String, String)>>, WebStorageError> {
        self.entries
            .lock()
            .map_err(|_| WebStorageError::StateUnavailable)
    }

    fn length(&self) -> Result<u32, WebStorageError> {
        u32::try_from(self.entries()?.len()).map_err(|_| WebStorageError::StateUnavailable)
    }

    fn key(&self, index: u32) -> Result<Option<String>, WebStorageError> {
        Ok(self
            .entries()?
            .get(index as usize)
            .map(|(key, _)| key.clone()))
    }

    fn get_item(&self, key: &str) -> Result<Option<String>, WebStorageError> {
        Ok(self
            .entries()?
            .iter()
            .find(|(stored, _)| stored == key)
            .map(|(_, value)| value.clone()))
    }

    fn set_item(&self, key: String, value: String) -> Result<(), WebStorageError> {
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

    fn remove_item(&self, key: &str) -> Result<(), WebStorageError> {
        self.entries()?.retain(|(stored, _)| stored != key);
        Ok(())
    }

    fn clear(&self) -> Result<(), WebStorageError> {
        self.entries()?.clear();
        Ok(())
    }
}

#[derive(Clone)]
enum StorageArea {
    Local(LocalStorage),
    Session(SessionStorage),
}

impl StorageArea {
    async fn length(&self) -> Result<u32, WebStorageError> {
        match self {
            Self::Local(storage) => storage.length().await,
            Self::Session(storage) => storage.length(),
        }
    }

    async fn key(&self, index: u32) -> Result<Option<String>, WebStorageError> {
        match self {
            Self::Local(storage) => storage.key(index).await,
            Self::Session(storage) => storage.key(index),
        }
    }

    async fn get_item(&self, key: &str) -> Result<Option<String>, WebStorageError> {
        match self {
            Self::Local(storage) => storage.get_item(key).await,
            Self::Session(storage) => storage.get_item(key),
        }
    }

    async fn set_item(&self, key: String, value: String) -> Result<(), WebStorageError> {
        match self {
            Self::Local(storage) => storage.set_item(&key, &value).await,
            Self::Session(storage) => storage.set_item(key, value),
        }
    }

    async fn remove_item(&self, key: &str) -> Result<(), WebStorageError> {
        match self {
            Self::Local(storage) => storage.remove_item(key).await,
            Self::Session(storage) => storage.remove_item(key),
        }
    }

    async fn clear(&self) -> Result<(), WebStorageError> {
        match self {
            Self::Local(storage) => storage.clear().await,
            Self::Session(storage) => storage.clear(),
        }
    }
}

struct WebStorageState {
    local: Option<LocalStorage>,
    session: SessionStorage,
}

impl WebStorageState {
    fn area(&self, persistent: bool) -> Result<StorageArea, WebStorageError> {
        if persistent {
            self.local
                .clone()
                .map(StorageArea::Local)
                .ok_or(WebStorageError::ContextNotSupported)
        } else {
            Ok(StorageArea::Session(self.session.clone()))
        }
    }
}

fn area(state: &Rc<RefCell<OpState>>, persistent: bool) -> Result<StorageArea, WebStorageError> {
    state.borrow().borrow::<WebStorageState>().area(persistent)
}

#[op2]
#[smi]
async fn op_webstorage_length(
    state: Rc<RefCell<OpState>>,
    persistent: bool,
) -> Result<u32, WebStorageError> {
    area(&state, persistent)?.length().await
}

#[op2]
#[string]
async fn op_webstorage_key(
    state: Rc<RefCell<OpState>>,
    persistent: bool,
    #[smi] index: u32,
) -> Result<Option<String>, WebStorageError> {
    area(&state, persistent)?.key(index).await
}

#[op2]
#[string]
async fn op_webstorage_get_item(
    state: Rc<RefCell<OpState>>,
    persistent: bool,
    #[string] key: String,
) -> Result<Option<String>, WebStorageError> {
    area(&state, persistent)?.get_item(&key).await
}

#[op2]
async fn op_webstorage_set_item(
    state: Rc<RefCell<OpState>>,
    persistent: bool,
    #[string] key: String,
    #[string] value: String,
) -> Result<(), WebStorageError> {
    area(&state, persistent)?.set_item(key, value).await
}

#[op2]
async fn op_webstorage_remove_item(
    state: Rc<RefCell<OpState>>,
    persistent: bool,
    #[string] key: String,
) -> Result<(), WebStorageError> {
    area(&state, persistent)?.remove_item(&key).await
}

#[op2]
async fn op_webstorage_clear(
    state: Rc<RefCell<OpState>>,
    persistent: bool,
) -> Result<(), WebStorageError> {
    area(&state, persistent)?.clear().await
}

deno_core::extension!(snowglass_webstorage,
    ops = [
        op_webstorage_length,
        op_webstorage_key,
        op_webstorage_get_item,
        op_webstorage_set_item,
        op_webstorage_remove_item,
        op_webstorage_clear,
    ],
    lazy_loaded_js = [dir "webstorage", "01_webstorage.js"],
    options = {
        local_storage: Option<LocalStorage>,
    },
    state = |state, options| {
        state.put(WebStorageState {
            local: options.local_storage,
            session: SessionStorage::default(),
        });
    },
);

pub(crate) fn extension(local_storage: Option<LocalStorage>) -> Extension {
    snowglass_webstorage::init(local_storage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn persists_values_across_connections() {
        let root = tempfile::tempdir().unwrap();
        let directories = Directories::rooted(root.path(), root.path());
        let storage = LocalStorage::open(&directories).await.unwrap();
        storage
            .set_item("Snowglass.theme", "chipotle")
            .await
            .unwrap();
        drop(storage);

        let storage = LocalStorage::open(&directories).await.unwrap();
        assert_eq!(
            storage.get_item("Snowglass.theme").await.unwrap(),
            Some("chipotle".to_owned())
        );
    }
}
