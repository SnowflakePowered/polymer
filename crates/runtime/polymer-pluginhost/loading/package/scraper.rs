use super::plugin::PluginPackage;
use crate::scraper::ScraperPluginInstance;
use polymer_pluginmanifest::{PluginManifest, ScrapeRequirement, ScraperBlock};

pub type ScraperPlugin = PluginPackage<ScraperBlock>;

impl ScraperPluginInstance for ScraperPlugin {
    fn requirements(&self) -> &[ScrapeRequirement] {
        PluginManifest::requirements(self)
    }

    fn optional(&self) -> &[ScrapeRequirement] {
        PluginManifest::optional(self)
    }

    fn provides(&self) -> &[String] {
        PluginManifest::provides(self)
    }
}
