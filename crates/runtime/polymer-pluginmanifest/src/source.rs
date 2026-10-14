use std::{
    borrow::Cow,
    collections::BTreeMap,
    io,
    ops::Range,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use crate::PackageError;

/// Where a plugin's files come from: an unpacked directory, or an asar archive held in memory.
#[derive(Clone, Debug)]
pub enum PackageSource {
    Directory(PathBuf),
    Archive(PackageArchive),
}

/// An asar archive read into memory, so the file it came from can be replaced while it is loaded.
#[derive(Clone)]
pub struct PackageArchive {
    path: PathBuf,
    bytes: Arc<[u8]>,
    files: Arc<BTreeMap<String, Range<usize>>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageEntry {
    File,
    Directory,
}

impl std::fmt::Debug for PackageArchive {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("PackageArchive")
            .field(&self.path)
            .finish()
    }
}

impl PackageArchive {
    /// Indexes the asar in `bytes`, read from `path`. Bytes after the archive are ignored.
    pub fn new(path: PathBuf, bytes: Arc<[u8]>) -> Result<Self, PackageError> {
        let invalid = |message: String| PackageError::Archive {
            path: path.clone(),
            message,
        };
        let (header, offset) =
            asar::Header::read(&mut &bytes[..]).map_err(|error| invalid(error.to_string()))?;
        Self::supported(&header).map_err(invalid)?;
        let reader = asar::AsarReader::new_from_header(header, offset, &bytes, None)
            .map_err(|error| invalid(error.to_string()))?;
        let base = bytes.as_ptr() as usize;
        let mut files = BTreeMap::new();
        for (name, file) in reader.files() {
            let key = name
                .components()
                .map(|component| match component {
                    Component::Normal(part) => part.to_str(),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| invalid(format!("invalid entry {}", name.display())))?
                .join("/");
            let start = (file.data().as_ptr() as usize)
                .checked_sub(base)
                .ok_or_else(|| invalid(format!("entry {key} is outside the archive")))?;
            files.insert(key, start..start + file.data().len());
        }
        Ok(Self {
            path,
            bytes,
            files: Arc::new(files),
        })
    }

    /// Rejects symbolic links and files stored outside the archive.
    fn supported(header: &asar::Header) -> Result<(), String> {
        match header {
            asar::Header::File(file) if file.unpacked() => {
                Err("the archive has entries stored outside it".to_owned())
            }
            asar::Header::File(_) => Ok(()),
            asar::Header::Link { .. } => Err("the archive has symbolic links".to_owned()),
            asar::Header::Directory { files } => files.values().try_for_each(Self::supported),
        }
    }

    /// The file the archive was read from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The whole file, including anything appended after the archive.
    pub fn bytes(&self) -> &Arc<[u8]> {
        &self.bytes
    }

    /// The contents of the file at `relative`, a `/`-separated path.
    pub fn file(&self, relative: &str) -> Option<&[u8]> {
        self.files
            .get(relative)
            .map(|range| &self.bytes[range.clone()])
    }

    /// Paths of every file in the archive.
    pub fn files(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(String::as_str)
    }

    fn entry(&self, relative: &str) -> Option<PackageEntry> {
        if self.files.contains_key(relative) {
            return Some(PackageEntry::File);
        }
        let prefix = format!("{relative}/");
        (relative.is_empty()
            || self
                .files
                .range(prefix.clone()..)
                .next()
                .is_some_and(|(key, _)| key.starts_with(&prefix)))
        .then_some(PackageEntry::Directory)
    }
}

impl PackageSource {
    /// An unpacked plugin directory.
    pub fn directory(path: impl AsRef<Path>) -> Result<Self, PackageError> {
        let path = path.as_ref();
        path.canonicalize()
            .map(Self::Directory)
            .map_err(|source| PackageError::Unreadable {
                path: path.to_owned(),
                source,
            })
    }

    /// The directory or archive file the package was read from.
    pub fn origin(&self) -> &Path {
        match self {
            Self::Directory(path) => path,
            Self::Archive(archive) => archive.path(),
        }
    }

    /// Converts a package-relative path to the `/`-separated form the other methods take.
    pub fn relative(path: &Path) -> Result<String, PackageError> {
        path.components()
            .filter(|component| *component != Component::CurDir)
            .map(|component| match component {
                Component::Normal(part) => part.to_str(),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()
            .filter(|parts| !parts.is_empty())
            .map(|parts| parts.join("/"))
            .ok_or_else(|| PackageError::EscapesPackage(path.to_owned()))
    }

    fn missing(&self, relative: &str) -> PackageError {
        PackageError::Unreadable {
            path: self.origin().join(relative),
            source: io::ErrorKind::NotFound.into(),
        }
    }

    /// The host path of `relative` in a directory package, confined to the package. Archives
    /// have no host paths.
    pub fn host_path(&self, relative: &str) -> Result<Option<PathBuf>, PackageError> {
        let Self::Directory(root) = self else {
            return Ok(None);
        };
        let path = root.join(relative);
        let path = path
            .canonicalize()
            .map_err(|source| PackageError::Unreadable { path, source })?;
        if !path.starts_with(root) {
            return Err(PackageError::EscapesPackage(path));
        }
        Ok(Some(path))
    }

    /// Whether `relative` is a file or directory in the package.
    pub fn entry(&self, relative: &str) -> Option<PackageEntry> {
        match self {
            Self::Directory(_) => self.host_path(relative).ok().flatten().and_then(|path| {
                let metadata = path.metadata().ok()?;
                Some(if metadata.is_dir() {
                    PackageEntry::Directory
                } else {
                    PackageEntry::File
                })
            }),
            Self::Archive(archive) => archive.entry(relative),
        }
    }

    /// Fails unless `relative` is a file in the package.
    pub fn require_file(&self, relative: &str) -> Result<(), PackageError> {
        match self.entry(relative) {
            Some(PackageEntry::File) => Ok(()),
            _ => Err(self.missing(relative)),
        }
    }

    pub fn read(&self, relative: &str) -> Result<Cow<'_, [u8]>, PackageError> {
        match self {
            Self::Directory(_) => {
                let path = self
                    .host_path(relative)?
                    .expect("directories have host paths");
                std::fs::read(&path)
                    .map(Cow::Owned)
                    .map_err(|source| PackageError::Unreadable { path, source })
            }
            Self::Archive(archive) => archive
                .file(relative)
                .map(Cow::Borrowed)
                .ok_or_else(|| self.missing(relative)),
        }
    }

    pub fn read_to_string(&self, relative: &str) -> Result<String, PackageError> {
        String::from_utf8(self.read(relative)?.into_owned()).map_err(|error| {
            PackageError::Unreadable {
                path: self.origin().join(relative),
                source: io::Error::new(io::ErrorKind::InvalidData, error),
            }
        })
    }

    /// Package-relative paths of the files matching the glob `pattern`, sorted.
    pub fn matching(&self, pattern: &str) -> Result<Vec<String>, PackageError> {
        let mut matches = match self {
            Self::Directory(root) => {
                let absolute = root.join(pattern);
                let pattern = absolute
                    .to_str()
                    .ok_or_else(|| PackageError::NonUtf8Path(absolute.clone()))?;
                glob::glob(pattern)?
                    .map(|path| Self::relative(path?.strip_prefix(root).unwrap_or(Path::new(""))))
                    .collect::<Result<Vec<_>, _>>()?
            }
            Self::Archive(archive) => {
                let pattern = glob::Pattern::new(pattern)?;
                let options = glob::MatchOptions {
                    require_literal_separator: true,
                    ..Default::default()
                };
                archive
                    .files()
                    .filter(|file| pattern.matches_with(file, options))
                    .map(str::to_owned)
                    .collect()
            }
        };
        matches.sort();
        Ok(matches)
    }
}
