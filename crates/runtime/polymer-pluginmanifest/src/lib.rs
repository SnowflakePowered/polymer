//! Plugin manifests (`plugin.toml`): parsing and validation, independent of the runtime that
//! executes plugins.

mod config;
mod emulator;
mod id;
mod manifest;
mod platform;
mod scraper;
mod source;
mod theme;

pub use config::{PluginConfig, PluginConfigError, PluginOption};
pub use emulator::{EmulatorBlock, SaveStateConfig};
pub use id::PluginId;
pub use manifest::{
    AnyPluginManifest, PackageError, PluginCache, PluginClass, PluginManifest, PluginMeta,
};
pub use platform::PlatformBlock;
pub use scraper::{ScrapeRequirement, ScraperBlock};
pub use source::{PackageArchive, PackageEntry, PackageSource};
pub use theme::{ThemeBlock, ThemeDebug};
