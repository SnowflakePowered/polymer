use super::ScrapeScalar;
use crate::runtime::PluginInvocationError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// A single object yielded by `onScrape`.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScrapeResult {
    pub key: String,
    pub value: ScrapeValue,
    pub source: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ScrapeValue {
    Scalar(ScrapeScalar),
    Inline(InlineMedia),
}

/// Binary media transported without fetching an external URL.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InlineMedia {
    pub kind: ScrapeMediaKind,
    pub media_type: String,
    pub bytes: Box<[u8]>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ScrapeMediaKind {
    Inline,
}

impl InlineMedia {
    pub fn accepts_type(key: &str, media_type: &str) -> bool {
        if key == "media.cue" {
            return media_type == "application/x-cue";
        }
        key.starts_with("media.")
            && ["image/", "audio/", "video/", "model/"]
                .iter()
                .any(|prefix| media_type.starts_with(prefix))
    }

    fn validate(&self, key: &str) -> Result<(), PluginInvocationError> {
        if !Self::accepts_type(key, &self.media_type) {
            return Err(PluginInvocationError::new(
                "TypeError",
                "INVALID_MEDIA_TYPE",
                "inline media has an unsupported MIME type for its key",
            ));
        }
        Ok(())
    }
}

impl ScrapeResult {
    /// Validate before retaining the next generator value in the worker.
    pub(crate) fn validate(
        &self,
        provided: &BTreeSet<String>,
    ) -> Result<(), PluginInvocationError> {
        if !provided.contains(&self.key) {
            return Err(PluginInvocationError::new(
                "TypeError",
                "UNDECLARED_SCRAPE_KEY",
                format!("scraper yielded undeclared key '{}'", self.key),
            ));
        }
        if self.source.trim().is_empty() {
            return Err(PluginInvocationError::new(
                "TypeError",
                "INVALID_SCRAPE_SOURCE",
                "scrape result source must not be empty",
            ));
        }
        match &self.value {
            ScrapeValue::Inline(media) => media.validate(&self.key),
            ScrapeValue::Scalar(value) => self.validate_scalar(value),
        }
    }

    fn validate_scalar(&self, value: &ScrapeScalar) -> Result<(), PluginInvocationError> {
        value.validate().map_err(|message| {
            PluginInvocationError::new("TypeError", "INVALID_SCRAPE_VALUE", message)
        })?;
        if !self.key.starts_with("media.") {
            return Ok(());
        }
        let ScrapeScalar::String(value) = value else {
            return Err(PluginInvocationError::new(
                "TypeError",
                "INVALID_MEDIA_URL",
                "media scrape values must be URL strings or inline media",
            ));
        };
        if url::Url::parse(value).is_ok_and(|url| matches!(url.scheme(), "http" | "https")) {
            return Ok(());
        }
        Err(PluginInvocationError::new(
            "TypeError",
            "INVALID_MEDIA_URL",
            "media scrape URLs must be absolute HTTP(S) URLs",
        ))
    }
}
