pub(crate) mod js;
mod result;

pub use result::{InlineMedia, ScrapeMediaKind, ScrapeResult, ScrapeValue};

pub use crate::loading::package::ScrapeRequirement;
use crate::plugin::PluginInstance;
pub use polymer_library::MetadataValue as ScrapeScalar;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One scalar value supplied to a scraper for a requested key.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrapeCandidate {
    pub source: String,
    pub value: ScrapeScalar,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ScrapeInput {
    pub metadata: BTreeMap<String, Vec<ScrapeCandidate>>,
    pub packages: Vec<serde_json::Value>,
}

impl From<BTreeMap<String, Vec<ScrapeCandidate>>> for ScrapeInput {
    fn from(metadata: BTreeMap<String, Vec<ScrapeCandidate>>) -> Self {
        Self {
            metadata,
            packages: Vec::new(),
        }
    }
}

impl ScrapeInput {
    pub fn satisfies(&self, requirement: &ScrapeRequirement) -> bool {
        requirement.keys().iter().any(|key| self.contains(key))
    }

    fn contains(&self, key: &str) -> bool {
        match key {
            "package.files" => self.packages.iter().any(|package| {
                package["files"]
                    .as_array()
                    .is_some_and(|files| !files.is_empty())
            }),
            "package" => !self.packages.is_empty(),
            _ => self
                .metadata
                .get(key)
                .is_some_and(|values| !values.is_empty()),
        }
    }
}


/// Scraper capability projected from a registered scripted plugin.
pub trait ScraperPluginInstance: PluginInstance {
    fn requirements(&self) -> &[ScrapeRequirement];
    fn optional(&self) -> &[ScrapeRequirement];
    fn provides(&self) -> &[String];

    fn accepts(&self, input: &ScrapeInput) -> bool {
        self.requirements()
            .iter()
            .all(|requirement| input.satisfies(requirement))
    }

    /// Whether this scraper still lacks `key` to satisfy one of its requirements.
    fn awaits(&self, input: &ScrapeInput, key: &str) -> bool {
        self.requirements()
            .iter()
            .chain(self.optional())
            .any(|requirement| {
                !input.satisfies(requirement)
                    && requirement.keys().iter().any(|wanted| wanted == key)
            })
    }

    fn requested_input(&self, input: &ScrapeInput) -> ScrapeInput {
        let metadata = self
            .requirements()
            .iter()
            .chain(self.optional())
            .flat_map(ScrapeRequirement::keys)
            .filter_map(|key| {
                input
                    .metadata
                    .get(key)
                    .map(|values| (key.clone(), values.clone()))
            })
            .collect();
        ScrapeInput {
            metadata,
            packages: input.packages.clone(),
        }
    }
}
