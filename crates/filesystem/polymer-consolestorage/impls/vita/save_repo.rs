use async_trait::async_trait;

use crate::impls::sony::save_repo::{SaveLayout, SonySaveRepo};
use crate::savegame::Package;
use crate::{Error, Storage, VersionedStorage};

/// One Polymer save volume, always mounted for the same guest user (00).
/// Choosing a different volume does not select a different Vita account.
pub type VitaSaveRepo = SonySaveRepo<VitaSaveLayout>;

#[derive(Debug)]
pub struct VitaSaveLayout;

#[async_trait]
impl SaveLayout for VitaSaveLayout {
    const PLATFORM: &'static str = super::VitaNand::PLATFORM;
    const SAVE_DIRECTORY: &'static str = "savedata";
    const SUB_TITLE: &'static str = "SUB_TITLE";
    const DETAIL: &'static str = "DETAIL";
    const HAS_STAGING: bool = true;

    async fn initialize(root: &VersionedStorage, profile_root: &str) -> Result<(), Error> {
        root.create_dir_all(&format!("{profile_root}/trophy"))
    }

    fn claims(profile_root: &str, _save_root: &str, path: &str) -> bool {
        path == profile_root
            || path
                .strip_prefix(profile_root)
                .is_some_and(|tail| tail.starts_with('/'))
    }

    fn package_id(package: &Package) -> Option<&str> {
        package.title_id().or_else(|| package.serial())
    }
}
