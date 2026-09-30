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

    /// The identifiable package: the file itself, or an artifact's installed package root.
    pub fn package_path(&self) -> io::Result<PathBuf> {
        match self {
            Self::File(path) => Ok(path.clone()),
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
                        root: checkout.join(entrypoint.root),
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
            _ => Err(invalid()),
        }
    }
}

impl fmt::Display for LaunchableUri {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(path) => fmt::Display::fmt(&path.display(), formatter),
            Self::Artifact(_) => {
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
