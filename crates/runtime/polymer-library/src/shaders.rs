use serde::Serialize;
use std::path::{Path, PathBuf};

/// A librashader preset found in the shader folder.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShaderPresetEntry {
    pub name: String,
    /// Folder of the preset relative to the shader folder, with `/` separators.
    pub category: String,
    /// Path of the preset relative to the shader folder, with `/` separators.
    pub relative_path: String,
    pub url: String,
}

/// The user's `shaders_slang` folder, scanned on every query so presets
/// added or removed on disk show up immediately.
#[derive(Debug, Clone)]
pub struct ShaderLibrary {
    root: PathBuf,
}

impl ShaderLibrary {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Presets whose name and folder contain every whitespace-separated term
    /// of `query`, best matches first.
    pub fn search(&self, query: &str, limit: usize) -> Vec<ShaderPresetEntry> {
        let terms = query
            .split_whitespace()
            .map(str::to_lowercase)
            .collect::<Vec<_>>();
        let mut matches = self
            .presets()
            .into_iter()
            .filter_map(|entry| {
                let name = entry.name.to_lowercase();
                let haystack = format!("{} {name}", entry.category.to_lowercase());
                terms
                    .iter()
                    .all(|term| haystack.contains(term.as_str()))
                    .then(|| {
                        let rank = terms.first().map_or(2, |first| {
                            if name.starts_with(first.as_str()) {
                                0
                            } else {
                                usize::from(!name.contains(first.as_str())) + 1
                            }
                        });
                        (rank, entry)
                    })
            })
            .collect::<Vec<_>>();
        matches.sort_by(|(left_rank, left), (right_rank, right)| {
            left_rank
                .cmp(right_rank)
                .then_with(|| left.name.len().cmp(&right.name.len()))
                .then_with(|| left.relative_path.len().cmp(&right.relative_path.len()))
                .then_with(|| left.relative_path.cmp(&right.relative_path))
        });
        matches
            .into_iter()
            .take(limit)
            .map(|(_, entry)| entry)
            .collect()
    }

    /// Every preset under the shader folder.
    pub fn presets(&self) -> Vec<ShaderPresetEntry> {
        let mut entries = Vec::new();
        let mut pending = vec![self.root.clone()];
        while let Some(directory) = pending.pop() {
            let Ok(children) = std::fs::read_dir(&directory) else {
                continue;
            };
            for child in children.flatten() {
                let path = child.path();
                if child.file_type().is_ok_and(|kind| kind.is_dir()) {
                    pending.push(path);
                } else if let Some(entry) = self.entry(&path) {
                    entries.push(entry);
                }
            }
        }
        entries
    }

    /// The preset at `relative_path`, if it still exists inside the shader folder.
    pub fn resolve(&self, relative_path: &str) -> Option<ShaderPresetEntry> {
        let relative = Path::new(relative_path);
        if relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return None;
        }
        let path = self.root.join(relative);
        path.is_file().then(|| self.entry(&path)).flatten()
    }

    fn entry(&self, path: &Path) -> Option<ShaderPresetEntry> {
        if !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("slangp"))
        {
            return None;
        }
        let relative = path.strip_prefix(&self.root).ok()?;
        let slash = |path: &Path| {
            path.components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/")
        };
        Some(ShaderPresetEntry {
            name: path.file_stem()?.to_string_lossy().into_owned(),
            category: relative.parent().map(slash).unwrap_or_default(),
            relative_path: slash(relative),
            url: url::Url::from_file_path(path).ok()?.to_string(),
        })
    }
}
