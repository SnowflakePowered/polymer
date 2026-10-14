use actix::Recipient;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    #[error("{0}")]
    Type(String),
    #[error("{0}")]
    Generic(String),
}

impl From<String> for LibraryError {
    fn from(error: String) -> Self {
        Self::Generic(error)
    }
}

impl From<&str> for LibraryError {
    fn from(error: &str) -> Self {
        Self::Generic(error.to_owned())
    }
}

/// Actix recipients exposed to Deno without depending on the runtime backend.
#[derive(Clone, Debug)]
pub struct Library {
    pub import: Recipient<LibraryImport>,
    pub import_key: Recipient<LibraryImportKey>,
    pub keys_list: Recipient<KeysList>,
    pub keys_platforms: Recipient<KeysPlatforms>,
    pub keys_licensed: Recipient<KeysLicensed>,
    pub keys_remove: Recipient<KeysRemove>,
    pub games_list: Recipient<GamesList>,
    pub games_platforms: Recipient<GamesPlatforms>,
    pub games_get: Recipient<GamesGet>,
    pub games_delete: Recipient<GamesDelete>,
    pub launchables_add: Recipient<LaunchablesAdd>,
    pub launchables_find: Recipient<LaunchablesFind>,
    pub launchables_delete: Recipient<LaunchablesDelete>,
    pub metadata_add: Recipient<MetadataAdd>,
    pub metadata_update: Recipient<MetadataUpdate>,
    pub metadata_delete: Recipient<MetadataDelete>,
    pub metadata_replace: Recipient<MetadataReplace>,
    pub metadata_text: Recipient<MetadataText>,
    pub metadata_url: Recipient<MetadataUrl>,
    pub scraping_start: Recipient<ScrapingStart>,
    pub scraping_scrapers: Recipient<ScrapingScrapers>,
    pub contents_refresh: Recipient<ContentsRefresh>,
    pub sources_list: Recipient<SourcesList>,
    pub sources_add: Recipient<SourcesAdd>,
    pub sources_remove: Recipient<SourcesRemove>,
    pub sources_scan: Recipient<SourcesScan>,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<Value, LibraryError>")]
pub struct SourcesList;

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<Value, LibraryError>")]
pub struct SourcesAdd {
    pub path: String,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<(), LibraryError>")]
pub struct SourcesRemove {
    pub id: String,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<(), LibraryError>")]
pub struct SourcesScan {
    pub id: String,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<Value, LibraryError>")]
pub struct LibraryImport {
    pub path: String,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<ImportedKey, LibraryError>")]
pub struct LibraryImportKey {
    pub platform: String,
    pub namespace: String,
    pub key: Vec<u8>,
    pub title_id: Option<String>,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<Value, LibraryError>")]
pub struct KeysList {
    pub platform: String,
    pub namespace: Option<String>,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<Vec<String>, LibraryError>")]
pub struct KeysPlatforms;

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<bool, LibraryError>")]
pub struct KeysLicensed {
    pub platform: String,
    pub title_id: String,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<bool, LibraryError>")]
pub struct KeysRemove {
    pub platform: String,
    pub namespace: String,
    pub id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedKey {
    pub namespace: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title_id: Option<String>,
    pub byte_length: usize,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<Value, LibraryError>")]
pub struct GamesList {
    pub query: polymer_library::GameQuery,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<Vec<String>, LibraryError>")]
pub struct GamesPlatforms;

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<Value, LibraryError>")]
pub struct GamesGet {
    pub id: String,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<(), LibraryError>")]
pub struct GamesDelete {
    pub id: String,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<Value, LibraryError>")]
pub struct LaunchablesAdd {
    pub path: String,
    pub platform: Option<String>,
    pub parent_binary_id: Option<String>,
    pub artifact: Option<InstalledArtifact>,
}

#[derive(Debug, serde::Deserialize)]
pub struct InstalledArtifact {
    pub storage: String,
    pub id: String,
    pub entrypoint: Option<String>,
    pub package: Option<String>,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<Value, LibraryError>")]
pub struct LaunchablesFind {
    pub uri: String,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<(), LibraryError>")]
pub struct LaunchablesDelete {
    pub id: String,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<Value, LibraryError>")]
pub struct MetadataAdd {
    pub parent_binary_id: String,
    pub input: Value,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<Value, LibraryError>")]
pub struct MetadataUpdate {
    pub id: String,
    pub input: Value,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<(), LibraryError>")]
pub struct MetadataDelete {
    pub id: String,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<Value, LibraryError>")]
pub struct MetadataReplace {
    pub parent_binary_id: String,
    pub source: String,
    pub inputs: Vec<Value>,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<String, LibraryError>")]
pub struct MetadataText {
    pub id: String,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<String, LibraryError>")]
pub struct MetadataUrl {
    pub id: String,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<Value, LibraryError>")]
pub struct ScrapingStart {
    pub root_binary_id: String,
    pub trigger_binary_id: String,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<Value, LibraryError>")]
pub struct ScrapingScrapers;

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<Value, LibraryError>")]
pub struct ContentsRefresh {
    pub plugin: String,
}
