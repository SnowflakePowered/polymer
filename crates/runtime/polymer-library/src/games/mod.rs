mod catalog;
mod jobs;
mod manifest;
mod model;
pub use manifest::{CuePart, CueTemplate, PackageFile, PackageManifest};
mod schema;
mod uri;
pub use uri::{LaunchableUri, ResolvedLaunchable};

pub use catalog::{GameCatalog, GameCatalogError};
pub use jobs::{ScrapeAttempt, ScrapeAttemptState, ScrapeJob, ScrapeJobState};
pub use model::{
    Game, IdentifiedLaunchable, Launchable, MediaData, MediaResource, Metadata, MetadataData,
    MetadataInput, MetadataInputData, MetadataValue,
};
