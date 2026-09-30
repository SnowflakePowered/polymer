use object_store::path::Path as ObjectPath;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq)]
pub struct Game {
    pub id: Uuid,
    pub platform: String,
    pub launchables: Vec<Launchable>,
    pub metadata: Vec<Metadata>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Launchable {
    pub id: Uuid,
    pub binary_id: Uuid,
    pub identified: IdentifiedLaunchable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdentifiedLaunchable {
    pub manifest: Option<super::PackageManifest>,
    pub path: super::LaunchableUri,
    pub entrypoint: Option<String>,
    pub package: Option<String>,
    pub binary_blake3: [u8; 32],
    pub serial: Option<String>,
    pub title_id: Option<String>,
    pub parent_binary_id: Option<Uuid>,
    pub platform: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MediaData {
    Object(ObjectPath),
    Inline(Box<[u8]>),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
pub enum MetadataValue {
    String(String),
    Number(f64),
    Boolean(bool),
}

impl MetadataValue {
    pub fn validate(&self) -> Result<(), &'static str> {
        if matches!(self, Self::Number(value) if !value.is_finite()) {
            return Err("metadata numbers must be finite");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum MetadataData {
    Scalar(MetadataValue),
    Media {
        media_type: String,
        data: MediaData,
        source_url: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Metadata {
    pub id: Uuid,
    pub parent_binary_id: Uuid,
    pub key: String,
    pub source: String,
    pub producer: String,
    pub ordinal: u32,
    pub data: MetadataData,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MetadataInput {
    pub key: String,
    pub source: String,
    #[serde(flatten)]
    pub data: MetadataInputData,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
pub enum MetadataInputData {
    Scalar {
        value: MetadataValue,
    },
    Inline {
        media_type: String,
        bytes: Box<[u8]>,
        #[serde(skip)]
        source_url: Option<String>,
    },
    Object {
        media_type: String,
        path: PathBuf,
        #[serde(skip)]
        source_url: Option<String>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaResource {
    pub metadata_id: Uuid,
    pub media_type: String,
    pub byte_length: u64,
    pub data: MediaData,
    pub content_blake3: [u8; 32],
}
