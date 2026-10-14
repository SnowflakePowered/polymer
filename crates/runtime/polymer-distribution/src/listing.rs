use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use polymer_pluginmanifest::PluginId;
use serde::Deserialize;

use crate::{Error, PluginHeader};

/// A repository's `Polymer.toml`: the plugins it publishes and their releases.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryManifest {
    #[serde(default)]
    pub plugin: Vec<PluginListing>,
}

/// A `[[plugin]]` table of `Polymer.toml`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginListing {
    pub id: String,
    pub class: String,
    /// Directory holding the plugin's `plugin.toml`, relative to `Polymer.toml`.
    pub path: PathBuf,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub icon: Option<PathBuf>,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub maintainers: Vec<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub release: Vec<Release>,
}

/// A `[[plugin.release]]` table: one signed archive of a plugin version.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Release {
    pub version: semver::Version,
    pub api: semver::VersionReq,
    pub url: String,
    #[serde(default)]
    pub yanked: bool,
}

impl RepositoryManifest {
    pub fn parse(text: &str) -> Result<Self, Error> {
        let manifest: Self = toml::from_str(text)?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn read(path: &Path) -> Result<Self, Error> {
        Self::parse(&std::fs::read_to_string(path)?)
    }

    pub fn listing(&self, id: &str) -> Option<&PluginListing> {
        self.plugin.iter().find(|listing| listing.id == id)
    }

    fn validate(&self) -> Result<(), Error> {
        let mut ids = BTreeSet::new();
        for listing in &self.plugin {
            if !ids.insert(&listing.id) {
                return Err(Error::Invalid(format!(
                    "plugin '{}' is listed twice",
                    listing.id
                )));
            }
            listing.validate()?;
        }
        Ok(())
    }
}

impl PluginListing {
    fn validate(&self) -> Result<(), Error> {
        PluginId::parse(&self.id).map_err(|error| Error::Invalid(error.to_string()))?;
        let mut versions = BTreeSet::new();
        for release in &self.release {
            if !versions.insert(&release.version) {
                return Err(Error::Invalid(format!(
                    "{} {} is released twice",
                    self.id, release.version
                )));
            }
            if !release.url.starts_with("https://") {
                return Err(Error::Invalid(format!(
                    "{} {} must be downloaded over https",
                    self.id, release.version
                )));
            }
        }
        Ok(())
    }

    /// Checks that a verified archive is `release` of this plugin.
    pub fn matches(&self, release: &Release, header: &PluginHeader) -> Result<(), Error> {
        if header.id != self.id
            || header.class != self.class
            || header.version != release.version
            || header.api != release.api
        {
            return Err(Error::Invalid(format!(
                "the archive is {} {} {} for API {}, not {} {} {} for API {}",
                header.class,
                header.id,
                header.version,
                header.api,
                self.class,
                self.id,
                release.version,
                release.api
            )));
        }
        Ok(())
    }
}
