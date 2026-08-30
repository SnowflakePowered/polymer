use deno_core::{Extension, OpState, op2};
use std::{cell::RefCell, rc::Rc};

mod local;
mod session;

pub use local::LocalStorage;
use session::SessionStorage;

const MAX_STORAGE_BYTES: usize = 10 * 1024 * 1024;

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

    fn from_op_state(
        state: &Rc<RefCell<OpState>>,
        persistent: bool,
    ) -> Result<StorageArea, WebStorageError> {
        state
            .borrow()
            .try_borrow::<Self>()
            .ok_or(WebStorageError::StateUnavailable)?
            .area(persistent)
    }
}

#[op2]
#[smi]
async fn op_webstorage_length(
    state: Rc<RefCell<OpState>>,
    persistent: bool,
) -> Result<u32, WebStorageError> {
    WebStorageState::from_op_state(&state, persistent)?
        .length()
        .await
}

#[op2]
#[string]
async fn op_webstorage_key(
    state: Rc<RefCell<OpState>>,
    persistent: bool,
    #[smi] index: u32,
) -> Result<Option<String>, WebStorageError> {
    WebStorageState::from_op_state(&state, persistent)?
        .key(index)
        .await
}

#[op2]
#[string]
async fn op_webstorage_get_item(
    state: Rc<RefCell<OpState>>,
    persistent: bool,
    #[string] key: String,
) -> Result<Option<String>, WebStorageError> {
    WebStorageState::from_op_state(&state, persistent)?
        .get_item(&key)
        .await
}

#[op2]
async fn op_webstorage_set_item(
    state: Rc<RefCell<OpState>>,
    persistent: bool,
    #[string] key: String,
    #[string] value: String,
) -> Result<(), WebStorageError> {
    WebStorageState::from_op_state(&state, persistent)?
        .set_item(key, value)
        .await
}

#[op2]
async fn op_webstorage_remove_item(
    state: Rc<RefCell<OpState>>,
    persistent: bool,
    #[string] key: String,
) -> Result<(), WebStorageError> {
    WebStorageState::from_op_state(&state, persistent)?
        .remove_item(&key)
        .await
}

#[op2]
async fn op_webstorage_clear(
    state: Rc<RefCell<OpState>>,
    persistent: bool,
) -> Result<(), WebStorageError> {
    WebStorageState::from_op_state(&state, persistent)?
        .clear()
        .await
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
