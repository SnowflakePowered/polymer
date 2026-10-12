use blitz_traits::net::Url;
use percent_encoding::percent_decode_str;
use polymer_library::Directories;
use std::path::PathBuf;

#[derive(Clone, Debug, Default)]
pub(crate) struct ThemeLocation {
    pub root: Option<PathBuf>,
    pub assets: Option<(String, PathBuf)>,
}

enum ThemeRoot {
    Directory(PathBuf),
    Remote(Url),
}

pub(crate) struct ShellUrls {
    theme_root: Option<ThemeRoot>,
    theme_assets: Option<(Vec<String>, PathBuf)>,
    directories: Directories,
}

impl ShellUrls {
    pub fn new(base_url: &str, theme: ThemeLocation, directories: Directories) -> Self {
        let base = Url::parse(base_url).and_then(|base| base.join("./")).ok();
        let theme_root = match (theme.root, base) {
            (Some(root), _) => Some(ThemeRoot::Directory(root)),
            (None, Some(base)) if base.scheme() == "file" => {
                base.to_file_path().ok().map(ThemeRoot::Directory)
            }
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

    fn theme(&self, url: &Url) -> Option<Url> {
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
        if let Some((prefix, directory)) = &self.theme_assets
            && segments.len() > prefix.len()
            && segments
                .iter()
                .zip(prefix)
                .all(|(segment, prefix)| segment.to_lowercase() == *prefix)
        {
            let file = segments[prefix.len()..].iter().collect::<PathBuf>();
            return Url::from_file_path(directory.join(file)).ok();
        }
        match self.theme_root.as_ref()? {
            ThemeRoot::Directory(root) => {
                Url::from_file_path(root.join(segments.iter().collect::<PathBuf>())).ok()
            }
            ThemeRoot::Remote(root) => root.join(path.trim_start_matches('/')).ok(),
        }
    }

    fn library(&self, url: &Url) -> Option<Url> {
        let digest = url.path().strip_prefix('/')?;
        let path =
            (url.host_str() == Some("media")).then(|| self.directories.library_media(digest))??;
        Url::from_file_path(path).ok()
    }
}
