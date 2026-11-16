//! Extraction of zip, 7z, and DMG archives into a host directory.

mod dmg;
mod sevenz;
mod zip;

use std::{
    fmt::Display,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("{} is not a zip, 7z, or DMG archive", .0.display())]
    UnsupportedFormat(PathBuf),
    #[error("archive entry '{0}' escapes the destination")]
    UnsafePath(String),
    #[error("invalid {format} archive: {message}")]
    Invalid { format: Format, message: String },
}

impl Error {
    fn invalid(format: Format, error: impl Display) -> Self {
        Self::Invalid {
            format,
            message: error.to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Format {
    Zip,
    SevenZip,
    Dmg,
}

impl Display for Format {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Zip => "zip",
            Self::SevenZip => "7z",
            Self::Dmg => "DMG",
        })
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ExtractOptions {
    /// Extract the contents of a single directory that holds every entry, instead of the directory.
    /// A macOS `.app` bundle is never stripped.
    pub strip_root: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Progress {
    pub completed: u64,
    pub total: u64,
}

/// A file or symbolic link in an archive.
struct Entry {
    path: String,
    size: u64,
}

trait Unarchiver {
    fn entries(&mut self) -> Result<Vec<Entry>, Error>;
    fn extract(&mut self, sink: &mut Sink<'_>) -> Result<(), Error>;
}

/// Writes extracted entries below the destination and reports progress.
struct Sink<'a> {
    destination: &'a Path,
    root: Option<String>,
    progress: &'a mut dyn FnMut(Progress),
    completed: u64,
    total: u64,
}

impl Sink<'_> {
    fn target(&self, name: &str) -> Result<Option<PathBuf>, Error> {
        let mut components = name
            .split(['/', '\\'])
            .filter(|component| !component.is_empty() && *component != ".")
            .peekable();
        if self
            .root
            .as_deref()
            .is_some_and(|root| components.peek() == Some(&root))
        {
            components.next();
        }
        let mut path = self.destination.to_path_buf();
        let mut named = false;
        for component in components {
            if component == ".." || component.contains(':') {
                return Err(Error::UnsafePath(name.to_owned()));
            }
            path.push(component);
            named = true;
        }
        Ok(named.then_some(path))
    }

    fn file(
        &mut self,
        name: &str,
        mode: Option<u32>,
        write: impl FnOnce(&mut File) -> Result<u64, Error>,
    ) -> Result<(), Error> {
        let Some(path) = self.target(name)? else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = File::create(&path)?;
        self.completed += write(&mut file)?;
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(mode & 0o777))?;
        }
        #[cfg(not(unix))]
        let _ = mode;
        (self.progress)(Progress {
            completed: self.completed.min(self.total),
            total: self.total,
        });
        Ok(())
    }

    fn symlink(&mut self, name: &str, target: &str) -> Result<(), Error> {
        let Some(path) = self.target(name)? else {
            return Ok(());
        };
        self.completed += target.len() as u64;
        let mut depth = path.strip_prefix(self.destination).map_or(0, |relative| {
            relative.components().count().saturating_sub(1)
        });
        let mut contained = !target.starts_with('/');
        for component in target.split('/') {
            match component {
                "" | "." => {}
                ".." if depth == 0 => contained = false,
                ".." => depth -= 1,
                _ => depth += 1,
            }
        }
        if !contained {
            tracing::debug!(%name, %target, "skipping symbolic link outside the archive");
            return Ok(());
        }
        #[cfg(unix)]
        {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::os::unix::fs::symlink(target, path)?;
        }
        #[cfg(not(unix))]
        tracing::debug!(%name, %target, "skipping symbolic link on a host without unprivileged links");
        Ok(())
    }
}

pub struct Archive {
    path: PathBuf,
    format: Format,
}

impl Archive {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, Error> {
        let path = path.into();
        let mut file = File::open(&path)?;
        let mut magic = [0; 8];
        let read = file.read(&mut magic)?;
        let magic = &magic[..read];
        let format = if magic.starts_with(b"PK\x03\x04") || magic.starts_with(b"PK\x05\x06") {
            Format::Zip
        } else if magic.starts_with(&[0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c]) {
            Format::SevenZip
        } else {
            let mut trailer = [0; 4];
            let length = file.seek(SeekFrom::End(0))?;
            if length < 512 {
                return Err(Error::UnsupportedFormat(path));
            }
            file.seek(SeekFrom::End(-512))?;
            file.read_exact(&mut trailer)?;
            if &trailer != b"koly" {
                return Err(Error::UnsupportedFormat(path));
            }
            Format::Dmg
        };
        Ok(Self { path, format })
    }

    pub fn format(&self) -> Format {
        self.format
    }

    pub fn extract(
        &self,
        destination: &Path,
        options: ExtractOptions,
        mut progress: impl FnMut(Progress),
    ) -> Result<(), Error> {
        let mut unarchiver: Box<dyn Unarchiver> = match self.format {
            Format::Zip => Box::new(zip::ZipUnarchiver::open(&self.path)?),
            Format::SevenZip => Box::new(sevenz::SevenZipUnarchiver::open(&self.path)?),
            Format::Dmg => Box::new(dmg::DmgUnarchiver::open(&self.path)?),
        };
        let entries = unarchiver.entries()?;
        let mut sink = Sink {
            destination,
            root: options
                .strip_root
                .then(|| Self::common_root(&entries))
                .flatten(),
            progress: &mut progress,
            completed: 0,
            total: entries.iter().map(|entry| entry.size).sum(),
        };
        std::fs::create_dir_all(destination)?;
        unarchiver.extract(&mut sink)
    }

    fn common_root(entries: &[Entry]) -> Option<String> {
        let mut root: Option<&str> = None;
        for entry in entries {
            let mut components = entry
                .path
                .split(['/', '\\'])
                .filter(|component| !component.is_empty() && *component != ".");
            let (Some(first), Some(_)) = (components.next(), components.next()) else {
                return None;
            };
            match root {
                Some(root) if root != first => return None,
                _ => root = Some(first),
            }
        }
        root.filter(|root| !root.ends_with(".app"))
            .map(str::to_owned)
    }
}
