use std::{
    fmt,
    hash::{Hash, Hasher},
    io,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, OnceLock},
};

use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

use super::GameCatalogError;
use crate::StorageManager;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum LaunchableUri {
    File(PathBuf),
    Artifact(InstalledArtifact),
    Mounted(MountedTitle),
}

/// Where a title lives inside the console storage a plugin mounts, so it can
/// boot it from there whether it was installed or mounted in place.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstalledLocation {
    pub device: String,
    pub path: String,
    pub entrypoint: String,
}

/// A title directory on the host, mounted at its install path in the
/// platform's managed NAND when launched.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct MountedTitle {
    pub platform: String,
    pub install_path: String,
    pub host: PathBuf,
    pub entrypoint: String,
}

#[derive(Clone, Debug)]
pub struct InstalledArtifact {
    pub platform: String,
    pub storage: Uuid,
    pub artifact: String,
    location: Arc<OnceLock<Option<ArtifactLocation>>>,
}

#[derive(Clone, Debug)]
struct ArtifactLocation {
    install_path: String,
    root: PathBuf,
    entrypoint: String,
    package: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedLaunchable {
    File(PathBuf),
    Directory {
        path: PathBuf,
        entrypoint: Option<String>,
    },
}

impl LaunchableUri {
    const FILE: &str = "file";
    const ARTIFACT: &str = "artifact";
    const MOUNT: &str = "mount";

    pub fn file(path: impl AsRef<Path>) -> Result<Self, GameCatalogError> {
        let path = dunce::simplified(path.as_ref());
        if !path.is_absolute() {
            return Err(GameCatalogError::InvalidData(
                "launchable file paths must be absolute".to_owned(),
            ));
        }
        Ok(Self::File(path.to_owned()))
    }

    pub fn artifact(platform: String, storage: Uuid, artifact: String) -> Self {
        Self::Artifact(InstalledArtifact {
            platform,
            storage,
            artifact,
            location: Arc::default(),
        })
    }

    fn location(&self) -> io::Result<(&Path, Option<&str>)> {
        match self {
            Self::File(path) => Ok((path, None)),
            Self::Mounted(mounted) => Ok((&mounted.host, Some(mounted.entrypoint.as_str()))),
            Self::Artifact(installed) => installed
                .location()
                .map(|location| (location.root.as_path(), Some(location.entrypoint.as_str())))
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("{} is not installed with a launch root", installed.artifact),
                    )
                }),
        }
    }

    pub fn host_path(&self) -> io::Result<PathBuf> {
        self.location().map(|(path, _)| path.to_owned())
    }

    /// The host path a library source can own: a file, or a mounted title directory.
    pub fn source_path(&self) -> Option<&Path> {
        match self {
            Self::File(path) => Some(path),
            Self::Mounted(mounted) => Some(&mounted.host),
            Self::Artifact(_) => None,
        }
    }

    pub fn installed(&self) -> Option<InstalledLocation> {
        let platform = match self {
            Self::File(_) => return None,
            Self::Mounted(mounted) => &mounted.platform,
            Self::Artifact(installed) => &installed.platform,
        };
        let (device, _) = polymer_consolestorage::ConsoleStorage::default_device(platform)?;
        let (_, entrypoint) = self.location().ok()?;
        Some(InstalledLocation {
            device: device.to_owned(),
            path: self.install_path()?.to_owned(),
            entrypoint: entrypoint?.to_owned(),
        })
    }

    /// Where the title lives inside its platform's NAND, if it lives in one.
    pub fn install_path(&self) -> Option<&str> {
        match self {
            Self::File(_) => None,
            Self::Mounted(mounted) => Some(&mounted.install_path),
            Self::Artifact(installed) => installed
                .location()
                .map(|location| location.install_path.as_str()),
        }
    }

    /// The identifiable package: the file itself, or an artifact's installed package root.
    pub fn package_path(&self) -> io::Result<PathBuf> {
        match self {
            Self::File(path) => Ok(path.clone()),
            Self::Mounted(mounted) => Ok(mounted.host.clone()),
            Self::Artifact(installed) => installed
                .location()
                .map(|location| location.package.clone())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!(
                            "{} is not installed with a package root",
                            installed.artifact
                        ),
                    )
                }),
        }
    }

    pub fn binary_path(&self) -> io::Result<PathBuf> {
        let (path, entrypoint) = self.location()?;
        Ok(entrypoint.map_or_else(|| path.to_owned(), |entrypoint| path.join(entrypoint)))
    }

    pub fn resolve(&self) -> io::Result<ResolvedLaunchable> {
        let (path, entrypoint) = self.location()?;
        let metadata = std::fs::metadata(path)?;
        if metadata.is_dir() {
            Ok(ResolvedLaunchable::Directory {
                path: path.to_owned(),
                entrypoint: entrypoint.map(str::to_owned),
            })
        } else if metadata.is_file() {
            Ok(ResolvedLaunchable::File(path.to_owned()))
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "launchable must be a regular file or directory",
            ))
        }
    }

    pub fn url(&self) -> Result<Url, GameCatalogError> {
        let invalid = || GameCatalogError::InvalidData(format!("{self:?} is not a valid URI"));
        match self {
            Self::File(path) => Url::from_file_path(path).map_err(|()| invalid()),
            Self::Mounted(mounted) => {
                let host = Url::from_file_path(&mounted.host).map_err(|()| invalid())?;
                let mut url = Url::parse(&format!(
                    "{}://{}/{}",
                    Self::MOUNT,
                    mounted.platform,
                    mounted.install_path
                ))
                .map_err(|_| invalid())?;
                url.query_pairs_mut()
                    .append_pair("host", host.as_str())
                    .append_pair("entrypoint", &mounted.entrypoint);
                Ok(url)
            }
            Self::Artifact(installed) => Url::parse(&format!(
                "{}://{}/{}/{}",
                Self::ARTIFACT,
                installed.platform,
                installed.storage.simple(),
                installed.artifact
            ))
            .map_err(|_| invalid()),
        }
    }

    fn from_mount_url(url: &Url) -> Option<Self> {
        let platform = url.host_str().filter(|platform| !platform.is_empty())?;
        let install_path = url
            .path()
            .strip_prefix('/')
            .filter(|path| !path.is_empty())?;
        let query = |key: &str| {
            url.query_pairs()
                .find_map(|(name, value)| (name == key).then(|| value.into_owned()))
        };
        let host = Url::parse(&query("host")?).ok()?.to_file_path().ok()?;
        Some(Self::Mounted(MountedTitle {
            platform: platform.to_owned(),
            install_path: install_path.to_owned(),
            host: dunce::simplified(&host).to_owned(),
            entrypoint: query("entrypoint").filter(|entrypoint| !entrypoint.is_empty())?,
        }))
    }

    fn from_artifact_url(url: &Url) -> Option<Self> {
        let platform = url.host_str().filter(|platform| !platform.is_empty())?;
        let mut segments = url.path_segments()?;
        let storage = Uuid::parse_str(segments.next()?).ok()?;
        let artifact = segments.next().filter(|artifact| !artifact.is_empty())?;
        segments
            .next()
            .is_none()
            .then(|| Self::artifact(platform.to_owned(), storage, artifact.to_owned()))
    }
}

impl InstalledArtifact {
    fn location(&self) -> Option<&ArtifactLocation> {
        self.location
            .get_or_init(|| {
                StorageManager::shared()
                    .and_then(|manager| {
                        manager.artifact_entrypoint(&self.platform, self.storage, &self.artifact)
                    })
                    .map(|(checkout, entrypoint)| ArtifactLocation {
                        root: checkout.join(&entrypoint.root),
                        install_path: entrypoint.root,
                        entrypoint: entrypoint.path,
                        package: checkout.join(entrypoint.package),
                    })
                    .inspect_err(|error| {
                        tracing::warn!(%error, platform = self.platform, storage = %self.storage, artifact = self.artifact, "installed artifact has no launch location");
                    })
                    .ok()
            })
            .as_ref()
    }
}

impl PartialEq for InstalledArtifact {
    fn eq(&self, other: &Self) -> bool {
        (&self.platform, self.storage, &self.artifact)
            == (&other.platform, other.storage, &other.artifact)
    }
}

impl Eq for InstalledArtifact {}

impl Hash for InstalledArtifact {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (&self.platform, self.storage, &self.artifact).hash(state);
    }
}

impl FromStr for LaunchableUri {
    type Err = GameCatalogError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid =
            || GameCatalogError::InvalidData(format!("`{value}` is not a launchable URI"));
        let url = Url::parse(value).map_err(|_| invalid())?;
        match url.scheme() {
            Self::FILE => Self::file(url.to_file_path().map_err(|()| invalid())?),
            Self::ARTIFACT => Self::from_artifact_url(&url).ok_or_else(invalid),
            Self::MOUNT => Self::from_mount_url(&url).ok_or_else(invalid),
            _ => Err(invalid()),
        }
    }
}

impl fmt::Display for LaunchableUri {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(path) => fmt::Display::fmt(&path.display(), formatter),
            Self::Artifact(_) | Self::Mounted(_) => {
                let url = self.url().map_err(|_| fmt::Error)?;
                formatter.write_str(url.as_str())
            }
        }
    }
}

impl Serialize for LaunchableUri {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let url = self.url().map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(url.as_str())
    }
}

impl<'de> Deserialize<'de> for LaunchableUri {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}
