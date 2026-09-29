use std::{
    fmt, io,
    path::{Path, PathBuf},
    str::FromStr,
};

use serde::{Deserialize, Serialize};
use url::Url;
use uuid::Uuid;

use super::GameCatalogError;
use crate::{Directories, StorageManager};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum LaunchableUri {
    File(PathBuf),
    Artifact {
        platform: String,
        storage: Uuid,
        artifact: String,
    },
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

    fn location(&self) -> io::Result<(PathBuf, Option<String>)> {
        match self {
            Self::File(path) => Ok((path.clone(), None)),
            Self::Artifact {
                platform,
                storage,
                artifact,
            } => Directories::discover()
                .and_then(|directories| {
                    StorageManager::new(directories.consolestorage())
                        .artifact_location(platform, *storage, artifact)
                })
                .map_err(io::Error::other),
        }
    }

    pub fn host_path(&self) -> io::Result<PathBuf> {
        self.location().map(|(path, _)| path)
    }

    pub fn binary_path(&self) -> io::Result<PathBuf> {
        let (path, entrypoint) = self.location()?;
        Ok(entrypoint.map_or_else(
            || path.clone(),
            |entrypoint| {
                entrypoint
                    .split('/')
                    .fold(path.clone(), |path, component| path.join(component))
            },
        ))
    }

    pub fn resolve(&self) -> io::Result<ResolvedLaunchable> {
        let (path, entrypoint) = self.location()?;
        let metadata = std::fs::metadata(&path)?;
        if metadata.is_dir() {
            Ok(ResolvedLaunchable::Directory { path, entrypoint })
        } else if metadata.is_file() {
            Ok(ResolvedLaunchable::File(path))
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "launchable must be a regular file or directory",
            ))
        }
    }

    fn url(&self) -> Result<Url, GameCatalogError> {
        let invalid = || GameCatalogError::InvalidData(format!("{self:?} is not a valid URI"));
        match self {
            Self::File(path) => Url::from_file_path(path).map_err(|()| invalid()),
            Self::Artifact {
                platform,
                storage,
                artifact,
            } => Url::parse(&format!(
                "{}://{platform}/{}/{artifact}",
                Self::ARTIFACT,
                storage.simple()
            ))
            .map_err(|_| invalid()),
        }
    }

    fn artifact(url: &Url) -> Option<Self> {
        let platform = url.host_str().filter(|platform| !platform.is_empty())?;
        let mut segments = url.path_segments()?;
        let storage = Uuid::parse_str(segments.next()?).ok()?;
        let artifact = segments.next().filter(|artifact| !artifact.is_empty())?;
        segments.next().is_none().then(|| Self::Artifact {
            platform: platform.to_owned(),
            storage,
            artifact: artifact.to_owned(),
        })
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
            Self::ARTIFACT => Self::artifact(&url).ok_or_else(invalid),
            _ => Err(invalid()),
        }
    }
}

impl fmt::Display for LaunchableUri {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let url = self.url().map_err(|_| fmt::Error)?;
        formatter.write_str(url.as_str())
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
