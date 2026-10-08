use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct KeyIdentity {
    pub namespace: String,
    pub id: String,
}

impl KeyIdentity {
    pub fn validate(&self) -> Result<(), KeyError> {
        if !matches!(self.namespace.as_str(), "system" | "content") || self.id.is_empty() {
            return Err(KeyError::Invalid(
                "expected system/content namespace and nonempty ID".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyRecord {
    #[serde(flatten)]
    pub identity: KeyIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_id: Option<String>,
    pub bytes: Vec<u8>,
}

impl KeyRecord {
    pub(crate) fn content(title_id: String, bytes: Vec<u8>) -> Self {
        Self {
            identity: KeyIdentity {
                namespace: "content".into(),
                id: title_id.clone(),
            },
            title_id: Some(title_id),
            bytes,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyDescription {
    #[serde(flatten)]
    pub identity: KeyIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_id: Option<String>,
    pub byte_length: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyQuery {
    pub namespace: Option<String>,
    pub title_id: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("key I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("key database: {0}")]
    Database(String),
    #[error("invalid key: {0}")]
    Invalid(String),
    #[error("key not found: {0}")]
    NotFound(String),
}

#[async_trait]
pub trait KeyReader: Send + Sync {
    async fn list(&self, query: &KeyQuery) -> Result<Vec<KeyDescription>, KeyError>;
    async fn read(&self, identity: &KeyIdentity) -> Result<KeyRecord, KeyError>;
}

#[async_trait]
pub trait KeyWriter: KeyReader {
    async fn put(&self, records: &[KeyRecord]) -> Result<(), KeyError>;
    async fn remove(&self, identity: &KeyIdentity) -> Result<bool, KeyError>;
}
