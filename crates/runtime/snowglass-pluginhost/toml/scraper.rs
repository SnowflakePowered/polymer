use super::plugin::{PackageError, PluginClass, PluginPackage, confined_path};
use crate::ScraperPluginInstance;
use serde::Deserialize;
use std::collections::BTreeSet;

#[derive(Debug, Deserialize)]
pub struct ScraperBlock {
    #[serde(default)]
    requires: Vec<ScrapeRequirement>,
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

pub type ScraperPlugin = PluginPackage<ScraperBlock>;

impl PluginClass for ScraperBlock {
    const CLASS: &'static str = "scraper";

    fn prepare(package: &mut PluginPackage<Self>) -> Result<(), PackageError> {
        confined_path(package.root(), package.main())?;
        let mut seen = BTreeSet::new();
        for key in &package.block().provides {
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

impl ScraperPluginInstance for PluginPackage<ScraperBlock> {
    fn requirements(&self) -> &[ScrapeRequirement] {
        &self.block().requires
    }
    fn provides(&self) -> &[String] {
        &self.block().provides
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requirements_parse_keys_and_any_groups() {
        assert_eq!(
            ScrapeRequirement::parse("package.platform").unwrap(),
            ScrapeRequirement::Key("package.platform".to_owned())
        );
        assert_eq!(
            ScrapeRequirement::parse("any(meta.title, filename.nointro.title)").unwrap(),
            ScrapeRequirement::Any(vec![
                "meta.title".to_owned(),
                "filename.nointro.title".to_owned(),
            ])
        );
    }

    #[test]
    fn requirements_reject_empty_or_malformed_groups() {
        for value in ["", "meta..title", "any(meta.title)", "any(meta.title, )"] {
            assert!(ScrapeRequirement::parse(value).is_err(), "accepted {value}");
        }
    }
}
