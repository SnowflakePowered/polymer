use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Error;
use crate::impls::ps3::depots::Ps3GameDirectory;
use crate::impls::vita::VitaDirectoryDepot;
use crate::impls::wiiu::depots::WiiUMetaXmlDepot;
use crate::installable::DepotType;
use crate::installable::artifacts::ArtifactEntrypoint;
use crate::keys::KeyRecord;

/// A title directory that runs from where it is, mounted read-only at its
/// install path instead of being installed.
pub trait InPlaceDepot: Sized {
    const PLATFORM: &'static str;
    /// Files that make a directory this depot, relative to its root.
    const MARKERS: &'static [&'static str];
    /// Whether managed updates can install beside it. PS3 updates replace the
    /// game directory itself, so an in-place PS3 game carries its own.
    const MANAGED_UPDATES: bool = true;

    fn locate(path: &Path) -> Result<Self, Error>;

    /// The base title whose components live at `install_path`.
    fn title_at(install_path: &str) -> Option<String>;

    /// The base title this depot belongs to.
    fn title(&self) -> String;

    fn kind(&self) -> DepotType;

    fn install_path(&self) -> String;

    fn entrypoint(&self) -> Result<Option<ArtifactEntrypoint>, Error>;

    fn keys(&self) -> Result<Vec<KeyRecord>, Error> {
        Ok(Vec::new())
    }
}

/// A host directory recognized by an [`InPlaceDepot`].
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InPlaceTitle {
    pub platform: String,
    pub title: String,
    pub kind: DepotType,
    pub install_path: String,
    pub entrypoint: Option<ArtifactEntrypoint>,
    #[serde(skip)]
    pub keys: Vec<KeyRecord>,
}

macro_rules! in_place_depots {
    ($($depot:ty),+ $(,)?) => {
        /// Recognize `path` as a title directory of any platform.
        pub fn locate(path: &Path) -> Result<Self, Error> {
            let mut errors = Vec::new();
            $(
                match <$depot>::locate(path) {
                    Ok(depot) => return Self::from_depot(depot),
                    Err(error) => errors.push(error.to_string()),
                }
            )+
            Err(Error::Unsupported(format!("not an in-place title: {}", errors.join("; "))))
        }

        /// The base title whose components live at `install_path` on `platform`.
        pub fn title_at(platform: &str, install_path: &str) -> Option<String> {
            $(if <$depot as InPlaceDepot>::PLATFORM == platform {
                return <$depot as InPlaceDepot>::title_at(install_path);
            })+
            None
        }

        /// Whether in-place titles on `platform` can take managed updates.
        pub fn managed_updates(platform: &str) -> bool {
            $(if <$depot as InPlaceDepot>::PLATFORM == platform {
                return <$depot as InPlaceDepot>::MANAGED_UPDATES;
            })+
            true
        }

        /// Files whose creation or removal can change what a directory is.
        pub fn markers() -> impl Iterator<Item = &'static str> {
            [$(<$depot as InPlaceDepot>::MARKERS),+].into_iter().flatten().copied()
        }
    };
}

impl InPlaceTitle {
    in_place_depots!(WiiUMetaXmlDepot, Ps3GameDirectory, VitaDirectoryDepot);

    fn from_depot<D: InPlaceDepot>(depot: D) -> Result<Self, Error> {
        Ok(Self {
            platform: D::PLATFORM.to_owned(),
            title: depot.title(),
            kind: depot.kind(),
            install_path: depot.install_path(),
            entrypoint: depot.entrypoint()?,
            keys: depot.keys()?,
        })
    }
}
