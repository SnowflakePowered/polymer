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

pub type ScrapeInput = BTreeMap<String, Vec<ScrapeCandidate>>;

/// Scraper capability projected from a registered scripted plugin.
pub trait ScraperPluginInstance: PluginInstance {
    fn requirements(&self) -> &[ScrapeRequirement];
    fn provides(&self) -> &[String];

    fn accepts(&self, input: &ScrapeInput) -> bool {
        self.requirements()
            .iter()
            .all(|requirement| match requirement {
                ScrapeRequirement::Key(key) => {
                    input.get(key).is_some_and(|values| !values.is_empty())
                }
                ScrapeRequirement::Any(keys) => keys
                    .iter()
                    .any(|key| input.get(key).is_some_and(|values| !values.is_empty())),
            })
    }

    fn requested_input(&self, input: &ScrapeInput) -> ScrapeInput {
        self.requirements()
            .iter()
            .flat_map(|requirement| match requirement {
                ScrapeRequirement::Key(key) => std::slice::from_ref(key).iter(),
                ScrapeRequirement::Any(keys) => keys.iter(),
            })
            .filter_map(|key| input.get(key).map(|values| (key.clone(), values.clone())))
            .collect()
    }
}
