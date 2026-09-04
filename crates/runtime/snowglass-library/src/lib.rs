pub mod directories;
pub mod games;
pub mod inputstore;
pub mod quickresume;
pub mod storage;

pub use directories::Directories;
pub use games::{
    Game, GameCatalog, GameCatalogError, IdentifiedLaunchable, Launchable, MediaData,
    MediaResource, Metadata, MetadataData, MetadataInput, MetadataInputData, MetadataValue,
    ScrapeAttempt, ScrapeAttemptState, ScrapeJob, ScrapeJobState,
};
pub use inputstore::{
    InputIdentityScope, InputMappingMode, InputMappingProfile, InputStore, InputStoreError,
};
pub use quickresume::{QuickResumeEntry, QuickResumeKey, QuickResumeStore};
pub use snowglass_consolestorage::nand::VirtualNand;
pub use snowglass_consolestorage::{ConsoleStorage, Nand, SaveRepo, StorageKind, VolumeInfo};
pub use storage::StorageManager;
