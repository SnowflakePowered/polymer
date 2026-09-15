pub mod directories;
pub mod games;
pub mod inputstore;
pub mod quickresume;
pub mod storage;
mod storage_selection;

pub use directories::Directories;
pub use games::{
    CuePart, CueTemplate, Game, GameCatalog, GameCatalogError, IdentifiedLaunchable, Launchable,
    MediaData, MediaResource, Metadata, MetadataData, MetadataInput, MetadataInputData,
    MetadataValue, PackageFile, PackageManifest, ScrapeAttempt, ScrapeAttemptState, ScrapeJob,
    ScrapeJobState,
};
pub use inputstore::{
    InputIdentityScope, InputMappingMode, InputMappingProfile, InputStore, InputStoreError,
};
pub use polymer_consolestorage::nand::VirtualNand;
pub use polymer_consolestorage::{ConsoleStorage, Nand, SaveRepo, StorageKind, VolumeInfo};
pub use quickresume::{QuickResumeEntry, QuickResumeKey, QuickResumeStore};
pub use storage::StorageManager;
pub use storage_selection::StorageSelection;
