use blitz_traits::net::Url;
use percent_encoding::percent_decode_str;
use polymer_library::Directories;
use polymer_pluginmanifest::PackageArchive;
use std::path::PathBuf;

/// Files of a theme: a host directory, or a directory inside an archived theme package.
#[derive(Clone, Debug)]
pub(crate) enum ThemeFiles {
    Directory(PathBuf),
    Archive(PackageArchive, String),
}

impl ThemeFiles {
    fn url(&self, url: &Url, segments: &[String]) -> Option<Url> {
        match self {
            Self::Directory(directory) => {
                Url::from_file_path(directory.join(segments.iter().collect::<PathBuf>())).ok()
            }
            Self::Archive(..) => self.file(segments).map(|_| url.clone()),
        }
    }

    fn file(&self, segments: &[String]) -> Option<&[u8]> {
        let Self::Archive(archive, directory) = self else {
            return None;
        };
        let path = std::iter::once(directory.as_str())
            .filter(|directory| !directory.is_empty())
            .chain(segments.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join("/");
        archive.file(&path)
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ThemeLocation {
    pub root: Option<ThemeFiles>,
    pub assets: Option<(String, ThemeFiles)>,
}

enum ThemeRoot {
    Files(ThemeFiles),
    Remote(Url),
}

pub(crate) struct ShellUrls {
    theme_root: Option<ThemeRoot>,
    theme_assets: Option<(Vec<String>, ThemeFiles)>,
    directories: Directories,
}

impl ShellUrls {
    pub fn new(base_url: &str, theme: ThemeLocation, directories: Directories) -> Self {
        let base = Url::parse(base_url).and_then(|base| base.join("./")).ok();
        let theme_root = match (theme.root, base) {
            (Some(root), _) => Some(ThemeRoot::Files(root)),
            (None, Some(base)) if base.scheme() == "file" => base
                .to_file_path()
                .ok()
                .map(|directory| ThemeRoot::Files(ThemeFiles::Directory(directory))),
            (None, base) => base.map(ThemeRoot::Remote),
        };
        let theme_assets = theme.assets.map(|(prefix, directory)| {
            let prefix = prefix
                .split('/')
                .filter(|segment| !segment.is_empty())
                .map(str::to_lowercase)
                .collect();
            (prefix, directory)
        });
        Self {
            theme_root,
            theme_assets,
            directories,
        }
    }

    pub fn directories(&self) -> &Directories {
        &self.directories
    }

    pub fn resolve(&self, url: &Url) -> Option<Url> {
        match url.scheme() {
            "theme" => self.theme(url),
            "library" => self.library(url),
            _ => Some(url.clone()),
        }
    }

    pub fn local_file(&self, url: &Url) -> Option<PathBuf> {
        if !matches!(url.scheme(), "theme" | "library") {
            return None;
        }
        self.resolve(url)
            .filter(|resolved| resolved.scheme() == "file")?
            .to_file_path()
            .ok()
    }

    /// The contents of a `theme:` URL that an archived theme serves.
    pub fn archived(&self, url: &Url) -> Option<Vec<u8>> {
        if url.scheme() != "theme" {
            return None;
        }
        let (_, segments) = self.theme_segments(url)?;
        match self.theme_files(&segments) {
            Some((files, rest)) => files.file(rest).map(<[u8]>::to_vec),
            None => None,
        }
    }

    /// The asset or root files serving `segments`, and the segments below them.
    fn theme_files<'a>(&self, segments: &'a [String]) -> Option<(&ThemeFiles, &'a [String])> {
        if let Some((prefix, files)) = &self.theme_assets
            && segments.len() > prefix.len()
            && segments
                .iter()
                .zip(prefix)
                .all(|(segment, prefix)| segment.to_lowercase() == *prefix)
        {
            return Some((files, &segments[prefix.len()..]));
        }
        match self.theme_root.as_ref()? {
            ThemeRoot::Files(files) => Some((files, segments)),
            ThemeRoot::Remote(_) => None,
        }
    }

    fn theme_segments(&self, url: &Url) -> Option<(String, Vec<String>)> {
        let path = format!("{}{}", url.host_str().unwrap_or_default(), url.path());
        let segments = path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(|segment| {
                let decoded = percent_decode_str(segment).decode_utf8().ok()?;
                let plain = !decoded.contains(['/', '\\', ':', '\0'])
                    && !decoded.trim_end_matches(['.', ' ']).is_empty();
                plain.then(|| decoded.into_owned())
            })
            .collect::<Option<Vec<_>>>()?;
        Some((path, segments))
    }

    fn theme(&self, url: &Url) -> Option<Url> {
        let (path, segments) = self.theme_segments(url)?;
        if let Some((files, rest)) = self.theme_files(&segments) {
            return files.url(url, rest);
        }
        match self.theme_root.as_ref()? {
            ThemeRoot::Remote(root) => root.join(path.trim_start_matches('/')).ok(),
            ThemeRoot::Files(_) => None,
        }
    }

    fn library(&self, url: &Url) -> Option<Url> {
        let digest = url.path().strip_prefix('/')?;
        let path =
            (url.host_str() == Some("media")).then(|| self.directories.library_media(digest))??;
        Url::from_file_path(path).ok()
    }
}
