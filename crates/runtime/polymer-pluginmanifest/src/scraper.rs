use serde::Deserialize;
use std::collections::BTreeSet;

use crate::{PackageError, PluginClass, PluginManifest};

#[derive(Debug, Deserialize)]
pub struct ScraperBlock {
    #[serde(default)]
    requires: Vec<ScrapeRequirement>,
    #[serde(default)]
    optional: Vec<ScrapeRequirement>,
    #[serde(default)]
    provides: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScrapeRequirement {
    Key(String),
    Any(Vec<String>),
}

impl std::fmt::Display for ScrapeRequirement {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Key(key) => formatter.write_str(key),
            Self::Any(keys) => write!(formatter, "any({})", keys.join(", ")),
        }
    }
}

impl PluginClass for ScraperBlock {
    const CLASS: &'static str = "scraper";

    fn prepare(manifest: &mut PluginManifest<Self>) -> Result<(), PackageError> {
        manifest.source().require_file(manifest.main())?;
        let mut seen = BTreeSet::new();
        for key in &manifest.block().provides {
            ScrapeRequirement::validate_key(key)?;
            if !seen.insert(key.clone()) {
                return Err(PackageError::DuplicateScrapeKey(key.clone()));
            }
        }
        Ok(())
    }
}

impl ScrapeRequirement {
    fn parse(value: &str) -> Result<Self, PackageError> {
        if let Some(inner) = value
            .strip_prefix("any(")
            .and_then(|value| value.strip_suffix(')'))
        {
            let keys = inner
                .split(',')
                .map(str::trim)
                .map(|key| {
                    Self::validate_key(key)?;
                    Ok(key.to_owned())
                })
                .collect::<Result<Vec<_>, PackageError>>()?;
            if keys.len() < 2 {
                return Err(PackageError::InvalidScrapeRequirement(value.to_owned()));
            }
            return Ok(Self::Any(keys));
        }
        Self::validate_key(value)?;
        Ok(Self::Key(value.to_owned()))
    }

    fn validate_key(key: &str) -> Result<(), PackageError> {
        if key.is_empty()
            || key.split('.').any(|segment| {
                segment.is_empty()
                    || !segment
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            })
        {
            return Err(PackageError::InvalidScrapeKey(key.to_owned()));
        }
        Ok(())
    }

    /// The keys any one of which satisfies the requirement.
    pub fn keys(&self) -> &[String] {
        match self {
            Self::Key(key) => std::slice::from_ref(key),
            Self::Any(keys) => keys,
        }
    }
}

impl<'de> Deserialize<'de> for ScrapeRequirement {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

impl PluginManifest<ScraperBlock> {
    pub fn requirements(&self) -> &[ScrapeRequirement] {
        &self.block().requires
    }

    pub fn optional(&self) -> &[ScrapeRequirement] {
        &self.block().optional
    }

    pub fn provides(&self) -> &[String] {
        &self.block().provides
    }
}
