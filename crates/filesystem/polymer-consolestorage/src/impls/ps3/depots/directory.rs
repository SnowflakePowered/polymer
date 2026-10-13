use std::fs::File;
use std::path::{Path, PathBuf};

use crate::Error;
use crate::impls::ps3::{Ps3Nand, TitleId};
use crate::impls::sony::metadata::SonyMetadata;
use crate::installable::artifacts::ArtifactEntrypoint;
use crate::installable::{DepotType, InPlaceDepot};

/// An installed HDD game directory, as found in another emulator's
/// `dev_hdd0/game`.
pub struct Ps3GameDirectory {
    title_id: String,
    install_path: String,
}

impl InPlaceDepot for Ps3GameDirectory {
    const PLATFORM: &'static str = Ps3Nand::PLATFORM;
    const MARKERS: &'static [&'static str] = &["PARAM.SFO"];
    const MANAGED_UPDATES: bool = false;

    fn locate(root: &Path) -> Result<Self, Error> {
        let path = root.join("PARAM.SFO");
        let sfo = SonyMetadata::read(
            File::open(&path).map_err(|error| Error::io(&path, error))?,
            &path.to_string_lossy(),
        )?;
        if sfo.get_string("CATEGORY") != Some("HG") {
            return Err(Error::Unsupported("not a PS3 HDD game".to_owned()));
        }
        let title_id = sfo.get_string("TITLE_ID").unwrap_or_default();
        if TitleId::parse(title_id).is_none() {
            return Err(Error::malformed(
                path.to_string_lossy(),
                "invalid PS3 TITLE_ID",
            ));
        }
        let eboot = PathBuf::from_iter([root, Path::new("USRDIR/EBOOT.BIN")]);
        if !eboot.is_file() {
            return Err(Error::io(&eboot, std::io::ErrorKind::NotFound.into()));
        }
        let directory = root
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| TitleId::parse(name).is_some())
            .unwrap_or(title_id);
        Ok(Self {
            title_id: title_id.to_owned(),
            install_path: format!("dev_hdd0/game/{directory}"),
        })
    }

    fn title_at(install_path: &str) -> Option<String> {
        install_path
            .strip_prefix("dev_hdd0/game/")
            .filter(|directory| !directory.contains('/'))
            .map(str::to_owned)
    }

    fn title(&self) -> String {
        self.title_id.clone()
    }

    fn kind(&self) -> DepotType {
        DepotType::App
    }

    fn install_path(&self) -> String {
        self.install_path.clone()
    }

    fn entrypoint(&self) -> Result<Option<ArtifactEntrypoint>, Error> {
        Ok(Some(ArtifactEntrypoint {
            root: self.install_path.clone(),
            path: "USRDIR/EBOOT.BIN".to_owned(),
            package: self.install_path.clone(),
        }))
    }
}
