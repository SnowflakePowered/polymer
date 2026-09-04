use std::io::{BufRead, BufReader};
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

/// Element text as a display string, with the line breaks Nintendo puts in
/// long names folded into single spaces.
#[derive(Debug, Default, Clone)]
pub struct Label(pub String);

impl<'de> Deserialize<'de> for Label {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let element = Text::deserialize(deserializer)?;

        Ok(Label(
            element
                .text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
        ))
    }
}

#[derive(Debug, Deserialize)]
pub struct MetaXml {
    pub title_id: HexBinary,
    #[serde(default)]
    pub title_version: Text,
    #[serde(default)]
    pub longname_en: Label,
    #[serde(default)]
    pub publisher_en: Label,
}

pub fn read<T: DeserializeOwned>(path: &Path) -> Result<T, Error> {
    let file = std::fs::File::open(path).map_err(|e| Error::io(path, e))?;
    read_from(BufReader::new(file), &path.display().to_string())
}

/// Deserialize a manifest straight from a reader, for sources that are not host
/// paths.
pub fn read_from<T: DeserializeOwned>(mut reader: impl BufRead, file: &str) -> Result<T, Error> {
    // Some of Nintendo's manifests are BOM-prefixed
    let bom = reader
        .fill_buf()
        .map(|head| head.starts_with(&[0xEF, 0xBB, 0xBF]))
        .unwrap_or(false);
    if bom {
        reader.consume(3);
    }

    quick_xml::de::from_reader(reader).map_err(|e| Error::MalformedMetadata {
        file: file.to_string(),
        reason: e.to_string(),
    })
}
