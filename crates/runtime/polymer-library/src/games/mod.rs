mod catalog;
mod manifest;
mod model;
pub use manifest::{CuePart, CueTemplate, PackageFile, PackageManifest};
mod schema;
mod uri;
pub use uri::{InstalledArtifact, LaunchableUri, ResolvedLaunchable};

pub use catalog::{GameCatalog, GameCatalogError};
pub use model::{
    Game, IdentifiedLaunchable, Launchable, MediaData, MediaResource, Metadata, MetadataData,
    MetadataInput, MetadataInputData, MetadataValue,
};
