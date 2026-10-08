use super::super::sony::save_repo::{SaveLayout, SonySaveRepo};
use crate::savegame::Package;
use crate::{Error, Storage, VersionedStorage};
use async_trait::async_trait;

pub type Ps3SaveRepo = SonySaveRepo<Ps3SaveLayout>;

#[derive(Debug)]
pub struct Ps3SaveLayout;

#[async_trait]
impl SaveLayout for Ps3SaveLayout {
    const PLATFORM: &'static str = super::Ps3Nand::PLATFORM;
    const SAVE_DIRECTORY: &'static str = "savedata";
    const SUB_TITLE: &'static str = "SUB_TITLE";
    const DETAIL: &'static str = "DETAIL";
    const HAS_STAGING: bool = true;

    async fn initialize(root: &VersionedStorage, profile_root: &str) -> Result<(), Error> {
        root.create_dir_all(&format!("{profile_root}/trophy"))?;
        root.install_file(
            Box::new(std::io::Cursor::new(b"User")),
            &format!("{profile_root}/localusername"),
        )
        .await?;
        Ok(())
    }

    fn claims(profile_root: &str, _save_root: &str, path: &str) -> bool {
        // Licenses remain owned by the system NAND.
        path.strip_prefix(profile_root)
            .and_then(|relative| relative.strip_prefix('/'))
            .is_some_and(|relative| relative.split('/').next() != Some("exdata"))
    }

    fn package_id(package: &Package) -> Option<&str> {
        package.title_id().or_else(|| package.serial())
    }
}
