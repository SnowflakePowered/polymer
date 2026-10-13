use polymer_consolestorage::installable::InPlaceTitle;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// What a directory is to a library source.
pub(super) enum DirectoryKind {
    Title(InPlaceTitle),
    Disc,
    Application,
    Folder,
}

/// How a library source divides its folder into importable units: in-place
/// titles and disc folders as whole directories, everything else as files.
/// Marker files only decide what their directory is and are never units.
pub(super) struct Discovery;

impl Discovery {
    const DISC_MARKERS: &[&str] = &["PS3_GAME/PARAM.SFO"];
    const APPLICATIONS: &[&str] = &["exe", "dll"];

    /// Every unit under `root`. Hidden folders and installed PC applications
    /// are skipped without being read.
    pub fn units(root: &Path) -> Vec<PathBuf> {
        let mut units = Vec::new();
        let mut walk = walkdir::WalkDir::new(root).into_iter();
        while let Some(entry) = walk.next() {
            let Ok(entry) = entry else {
                continue;
            };
            let path = entry.path();
            if !entry.file_type().is_dir() {
                if Self::marked(path).is_none() {
                    units.push(path.to_owned());
                }
                continue;
            }
            if entry.depth() > 0 && (Self::hidden(path) || Self::application(path)) {
                walk.skip_current_dir();
            } else if Self::directory_unit(path) {
                units.push(path.to_owned());
                walk.skip_current_dir();
            }
        }
        units
    }

    /// The unit a changed path belongs to, or `None` inside an ignored folder.
    pub fn unit_of(root: &Path, path: &Path) -> Option<PathBuf> {
        let mut unit = path.to_owned();
        for ancestor in path.ancestors().skip(1) {
            if !ancestor.starts_with(root) {
                break;
            }
            if ancestor != root && (Self::hidden(ancestor) || Self::application(ancestor)) {
                return None;
            }
            if Self::directory_unit(ancestor) {
                unit = ancestor.to_owned();
            }
        }
        Some(unit)
    }

    /// The directory whose kind a marker file decides. Longer markers win, so
    /// `PS3_GAME/PARAM.SFO` names the disc folder rather than `PS3_GAME`.
    pub fn marked(path: &Path) -> Option<PathBuf> {
        let mut markers: Vec<_> = InPlaceTitle::markers()
            .chain(Self::DISC_MARKERS.iter().copied())
            .collect();
        markers.sort_by_key(|marker| std::cmp::Reverse(marker.split('/').count()));
        markers.into_iter().find_map(|marker| {
            let depth = marker.split('/').count();
            let suffix: Vec<_> = path.components().rev().take(depth).collect();
            let matches = suffix.len() == depth
                && suffix
                    .iter()
                    .rev()
                    .zip(marker.split('/'))
                    .all(|(part, name)| part.as_os_str().eq_ignore_ascii_case(name));
            matches.then(|| path.ancestors().nth(depth).map(Path::to_owned))?
        })
    }

    pub fn classify(path: &Path, root: &Path) -> DirectoryKind {
        if let Ok(title) = InPlaceTitle::locate(path) {
            DirectoryKind::Title(title)
        } else if Self::disc(path) {
            DirectoryKind::Disc
        } else if path != root && Self::application(path) {
            DirectoryKind::Application
        } else {
            DirectoryKind::Folder
        }
    }

    /// Size and modification time of `files`, which change whenever the unit
    /// they identify is replaced.
    pub fn stamp(files: impl IntoIterator<Item = PathBuf>) -> String {
        files
            .into_iter()
            .map(|file| match std::fs::metadata(&file) {
                Ok(metadata) => format!(
                    "{}:{}",
                    metadata.len(),
                    metadata
                        .modified()
                        .ok()
                        .and_then(|modified| modified.duration_since(SystemTime::UNIX_EPOCH).ok())
                        .map_or(0, |modified| modified.as_nanos())
                ),
                Err(_) => "-".to_owned(),
            })
            .collect::<Vec<_>>()
            .join(";")
    }

    /// The files whose stamps identify a directory unit.
    pub fn identity(path: &Path, entrypoint: Option<&str>) -> Vec<PathBuf> {
        InPlaceTitle::markers()
            .chain(Self::DISC_MARKERS.iter().copied())
            .map(str::to_owned)
            .chain(entrypoint.map(str::to_owned))
            .map(|file| path.join(file))
            .filter(|file| file.is_file())
            .collect()
    }

    fn directory_unit(path: &Path) -> bool {
        Self::disc(path) || InPlaceTitle::locate(path).is_ok()
    }

    fn disc(path: &Path) -> bool {
        Self::DISC_MARKERS
            .iter()
            .any(|marker| path.join(marker).is_file())
    }

    fn application(path: &Path) -> bool {
        std::fs::read_dir(path)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .path()
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| {
                        Self::APPLICATIONS
                            .iter()
                            .any(|known| extension.eq_ignore_ascii_case(known))
                    })
            })
    }

    fn hidden(path: &Path) -> bool {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.starts_with('.')
                    || name.eq_ignore_ascii_case("$RECYCLE.BIN")
                    || name.eq_ignore_ascii_case("System Volume Information")
            })
    }
}
