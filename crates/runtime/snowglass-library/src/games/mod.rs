mod catalog;
mod model;
mod schema;

pub use catalog::{GameCatalog, GameCatalogError};
pub use model::{
    Game, IdentifiedLaunchable, Launchable, MediaData, MediaResource, Metadata, MetadataData,
    MetadataInput, Relation,
};
