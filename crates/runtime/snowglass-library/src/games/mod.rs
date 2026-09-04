mod catalog;
mod jobs;
mod model;
mod schema;

pub use catalog::{GameCatalog, GameCatalogError};
pub use jobs::{ScrapeAttempt, ScrapeAttemptState, ScrapeJob, ScrapeJobState};
pub use model::{
    Game, IdentifiedLaunchable, Launchable, MediaData, MediaResource, Metadata, MetadataData,
    MetadataInput, MetadataInputData, MetadataValue,
};
