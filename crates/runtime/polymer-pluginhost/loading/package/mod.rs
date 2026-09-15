mod emulator;
mod plugin;
mod scraper;
mod theme;

pub(crate) use emulator::EmulatorPlugin;
pub use emulator::SaveStateConfig;
pub use plugin::PackageError;
pub use plugin::PluginCache;
pub use scraper::ScrapeRequirement;
pub(crate) use scraper::ScraperPlugin;
pub use theme::ThemeDebug;
pub(crate) use theme::ThemePlugin;

mod platform;
pub(crate) use platform::PlatformPlugin;
