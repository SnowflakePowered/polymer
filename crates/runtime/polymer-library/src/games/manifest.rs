use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A disc representation belongs to a launchable, independently of binary identity.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageManifest {
    pub version: u32,
    pub revision: String,
    pub descriptor: PackageFile,
    pub cue: String,
    pub files: Vec<PackageFile>,
    pub template: Option<CueTemplate>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageFile {
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
    pub modified_ns: String,
}

/// Text fragments preserve original formatting and comments; slots replace operands only.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct CueTemplate {
    pub parts: Vec<CuePart>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CuePart {
    Text(String),
    File(usize),
}
