use std::path::PathBuf;

use object_store::path::Path as ObjectPath;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Eq, PartialEq)]
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
    pub path: PathBuf,
    pub binary_blake3: Box<[u8]>,
    pub serial: Option<String>,
    pub title_id: Option<String>,
    pub parent_binary_id: Option<Uuid>,
    pub platform: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdentifiedLaunchable {
    pub path: PathBuf,
    pub binary_blake3: [u8; 32],
    pub serial: Option<String>,
    pub title_id: Option<String>,
    pub parent_binary_id: Option<Uuid>,
    pub platform: String,
}

#[non_exhaustive]
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Relation {
    Title,
    Description,
    ReleaseDate,
    Publisher,
    BoxartFront,
    BoxartBack,
    BoxartSpine,
    BoxartFull,
    Screenshot,
    Logo,
    Video,
    Icon,
    Banner,
    Media,
}

impl Relation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Description => "description",
            Self::ReleaseDate => "releasedate",
            Self::Publisher => "publisher",
            Self::BoxartFront => "boxartfront",
            Self::BoxartBack => "boxartback",
            Self::BoxartSpine => "boxartspine",
            Self::BoxartFull => "boxartfull",
            Self::Screenshot => "screenshot",
            Self::Logo => "logo",
            Self::Video => "video",
            Self::Icon => "icon",
            Self::Banner => "banner",
            Self::Media => "media",
        }
    }

    pub const fn expects_text(self) -> bool {
        matches!(self, Self::Title | Self::Description | Self::Publisher)
    }

    pub const fn expects_year(self) -> bool {
        matches!(self, Self::ReleaseDate)
    }
}

impl std::str::FromStr for Relation {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value {
            "title" => Self::Title,
            "description" => Self::Description,
            "releasedate" => Self::ReleaseDate,
            "publisher" => Self::Publisher,
            "boxartfront" => Self::BoxartFront,
            "boxartback" => Self::BoxartBack,
            "boxartspine" => Self::BoxartSpine,
            "boxartfull" => Self::BoxartFull,
            "screenshot" => Self::Screenshot,
            "logo" => Self::Logo,
            "video" => Self::Video,
            "icon" => Self::Icon,
            "banner" => Self::Banner,
            "media" => Self::Media,
            _ => return Err(format!("unknown metadata relation '{value}'")),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MediaData {
    Object(ObjectPath),
    Inline(Box<[u8]>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MetadataData {
    Text(String),
    ReleaseYear(u16),
    Media { media_type: String, data: MediaData },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Metadata {
    pub id: Uuid,
    pub parent_binary_id: Uuid,
    pub data: MetadataData,
    pub relation: Relation,
    pub language: Option<String>,
    pub source: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MetadataInput {
    Text {
        relation: Relation,
        language: Option<String>,
        source: String,
        value: String,
    },
    ReleaseYear {
        language: Option<String>,
        source: String,
        year: u16,
    },
    Inline {
        relation: Relation,
        language: Option<String>,
        source: String,
        media_type: String,
        bytes: Box<[u8]>,
    },
    Object {
        relation: Relation,
        language: Option<String>,
        source: String,
        media_type: String,
        path: PathBuf,
    },
}

impl MetadataInput {
    pub fn with_source(mut self, source: &str) -> Self {
        match &mut self {
            Self::Text { source: value, .. }
            | Self::ReleaseYear { source: value, .. }
            | Self::Inline { source: value, .. }
            | Self::Object { source: value, .. } => source.clone_into(value),
        }
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaResource {
    pub metadata_id: Uuid,
    pub media_type: String,
    pub byte_length: u64,
    pub data: MediaData,
    pub content_blake3: [u8; 32],
}
