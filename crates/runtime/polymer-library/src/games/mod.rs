mod catalog;
mod discs;
mod manifest;
mod model;
mod preview;
pub use preview::MediaPreview;
pub use manifest::{CuePart, CueTemplate, PackageFile, PackageManifest};
mod schema;
mod uri;
pub use uri::{InstalledArtifact, LaunchableUri, ResolvedLaunchable};

pub use catalog::{GameCatalog, GameCatalogError};
pub use discs::{DiscSets, Serial};
pub use model::{
    Game, GamePage, GameQuery, GameSort, IdentifiedLaunchable, Launchable, MediaData,
    MediaResource, Metadata, MetadataData, MetadataInput, MetadataInputData, MetadataValue,
    PlatformSettings,
};
