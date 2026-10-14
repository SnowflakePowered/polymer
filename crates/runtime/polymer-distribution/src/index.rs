use std::path::{Path, PathBuf};

use polymer_pluginmanifest::PluginId;
use serde::Deserialize;

use crate::Error;

/// A repository's file in the polymer-index repository.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexEntry {
    /// `owner/name` of the GitHub repository holding `Polymer.toml`.
    pub repo: String,
    /// Branch or tag `Polymer.toml` is read from.
    #[serde(rename = "ref", default = "IndexEntry::default_ref")]
    pub reference: String,
    /// Minisign public keys that sign the repository's releases.
    pub keys: Vec<String>,
    /// Official repositories publish unscoped plugin IDs.
    #[serde(default)]
    pub official: bool,
    /// Scopes this repository claims; it publishes `@scope/name` IDs under them.
    #[serde(default)]
    pub scopes: Vec<String>,
}

impl IndexEntry {
    fn default_ref() -> String {
        "main".to_owned()
    }

    pub fn read(path: &Path) -> Result<Self, Error> {
        let entry: Self = toml::from_str(&std::fs::read_to_string(path)?)?;
        entry.validate()?;
        Ok(entry)
    }

    fn validate(&self) -> Result<(), Error> {
        let parts = self.repo.split('/').collect::<Vec<_>>();
        if parts.len() != 2 || parts.iter().any(|part| part.is_empty()) {
            return Err(Error::Invalid(format!(
                "repo '{}' is not 'owner/name'",
                self.repo
            )));
        }
        if self.keys.is_empty() {
            return Err(Error::Invalid(format!(
                "{} lists no signing keys",
                self.repo
            )));
        }
        for key in &self.keys {
            minisign_verify::PublicKey::from_base64(key)
                .map_err(|error| Error::Invalid(format!("signing key '{key}': {error}")))?;
        }
        if let Some(scope) = self
            .scopes
            .iter()
            .find(|scope| !PluginId::is_valid_name(scope))
        {
            return Err(Error::Invalid(format!(
                "{} claims invalid scope '{scope}'",
                self.repo
            )));
        }
        if !self.official && self.scopes.is_empty() {
            return Err(Error::Invalid(format!(
                "{} is not official and claims no scopes",
                self.repo
            )));
        }
        Ok(())
    }

    /// Whether this repository may publish the plugin `id`.
    pub fn publishes(&self, id: &str) -> bool {
        match PluginId::parse(id) {
            Ok(PluginId {
                scope: Some(scope), ..
            }) => self.scopes.iter().any(|claimed| claimed == scope),
            Ok(PluginId { scope: None, .. }) => self.official,
            Err(_) => false,
        }
    }

    /// The parsed signing keys.
    pub fn public_keys(&self) -> Vec<minisign_verify::PublicKey> {
        self.keys
            .iter()
            .filter_map(|key| minisign_verify::PublicKey::from_base64(key).ok())
            .collect()
    }
}

/// The entries of a polymer-index checkout: `repos/<owner>/<repo>.toml`.
#[derive(Clone, Debug, Default)]
pub struct Index {
    pub entries: Vec<IndexEntry>,
    /// Entry files that could not be read, and why. They are left out of the index.
    pub rejected: Vec<(PathBuf, String)>,
}

impl Index {
    /// Reads every entry under `root/repos`. A missing directory is an empty index.
    pub fn read(root: &Path) -> Result<Self, Error> {
        let repos = root.join("repos");
        if !repos.is_dir() {
            return Ok(Self::default());
        }
        let mut index = Self::default();
        for owner in std::fs::read_dir(repos)? {
            let owner = owner?;
            if !owner.file_type()?.is_dir() {
                continue;
            }
            for file in std::fs::read_dir(owner.path())? {
                let path = file?.path();
                if !path
                    .extension()
                    .is_some_and(|extension| extension == "toml")
                {
                    continue;
                }
                match IndexEntry::read(&path) {
                    Ok(entry) => index.entries.push(entry),
                    Err(error) => index.rejected.push((path, error.to_string())),
                }
            }
        }
        Ok(index)
    }

    /// The keys that may sign plugin `id`. A scope claimed by more than one repository
    /// trusts no one.
    pub fn keys(&self, id: &str) -> Vec<minisign_verify::PublicKey> {
        let publishers = self
            .entries
            .iter()
            .filter(|entry| entry.publishes(id))
            .collect::<Vec<_>>();
        match PluginId::parse(id) {
            Ok(PluginId { scope: Some(_), .. }) if publishers.len() > 1 => Vec::new(),
            _ => publishers
                .iter()
                .flat_map(|entry| entry.public_keys())
                .collect(),
        }
    }
}
