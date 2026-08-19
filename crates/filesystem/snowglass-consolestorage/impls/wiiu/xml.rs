use std::path::Path;

use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::error::Error;

/// A `hexBinary` element such as `<title_id type="hexBinary" length="8">…</title_id>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HexBinary(pub u64);

impl<'de> Deserialize<'de> for HexBinary {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let element = Text::deserialize(deserializer)?;
        u64::from_str_radix(element.text.trim(), 16)
            .map(HexBinary)
            .map_err(serde::de::Error::custom)
    }
}

/// Any element read for its text content.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct Text {
    #[serde(rename = "$text", default)]
    pub text: String,
}

#[derive(Debug, Deserialize)]
pub struct AppXml {
    pub title_id: HexBinary,
}

#[derive(Debug, Deserialize)]
pub struct MetaXml {
    pub title_id: HexBinary,
    #[serde(default)]
    pub title_version: Text,
    #[serde(default)]
    pub longname_en: Text,
}

pub fn read<T: DeserializeOwned>(path: &Path) -> Result<T, Error> {
    let source = std::fs::read_to_string(path).map_err(|e| Error::io(path, e))?;
    quick_xml::de::from_str(source.trim_start_matches('\u{feff}')).map_err(|e| {
        Error::MalformedMetadata {
            file: path.display().to_string(),
            reason: e.to_string(),
        }
    })
}
