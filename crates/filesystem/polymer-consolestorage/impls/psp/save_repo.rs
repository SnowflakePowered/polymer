use super::super::sony::save_repo::{SaveLayout, SonySaveRepo};
use crate::savegame::Package;
use crate::{Error, VersionedStorage};
use async_trait::async_trait;

pub type PspSaveRepo = SonySaveRepo<PspSaveLayout>;

#[derive(Debug)]
pub struct PspSaveLayout;

#[async_trait]
impl SaveLayout for PspSaveLayout {
    const PLATFORM: &'static str = super::PspMemoryStick::PLATFORM;
    const SAVE_DIRECTORY: &'static str = "SAVEDATA";
    const SUB_TITLE: &'static str = "SAVEDATA_TITLE";
    const DETAIL: &'static str = "SAVEDATA_DETAIL";
    const HAS_STAGING: bool = false;

    async fn initialize(_root: &VersionedStorage, _profile_root: &str) -> Result<(), Error> {
        Ok(())
    }

    fn claims(_profile_root: &str, save_root: &str, path: &str) -> bool {
        path == save_root
            || path
                .strip_prefix(save_root)
                .is_some_and(|relative| relative.starts_with('/'))
    }

    fn package_id(package: &Package) -> Option<&str> {
        package.serial().or_else(|| package.title_id())
    }
}
