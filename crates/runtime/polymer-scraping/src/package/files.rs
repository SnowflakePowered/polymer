use super::Package;
use std::path::Path;

impl Package {
    /// Scraper file objects retain their own identity and size. No content is
    /// read until the plugin asks for a digest.
    pub(crate) fn scrape_value(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(self).expect("package serialization");
        let paths: Vec<_> = match &self.manifest {
            Some(manifest) => manifest
                .files
                .iter()
                .map(|file| file.path.as_path())
                .collect(),
            None => vec![self.path.as_path()],
        };
        let files: Vec<_> = paths
            .into_iter()
            .filter_map(|path| {
                if Self::is_container(path) {
                    return None;
                }
                let metadata = std::fs::metadata(path).ok()?;
                if !metadata.is_file() {
                    return None;
                }
                Some(serde_json::json!({
                    "__digestPath": path,
                    "name": path.file_name().unwrap_or_default().to_string_lossy(),
                    "size": metadata.len(),
                }))
            })
            .collect();
        value["files"] = files.into();
        value
    }

    fn is_container(path: &Path) -> bool {
        let extension = path
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        // Container digests cannot identify a decompressed dump or a disc track.
        matches!(
            extension.as_str(),
            "zip"
                | "7z"
                | "rar"
                | "gz"
                | "xz"
                | "zst"
                | "chd"
                | "cso"
                | "zso"
                | "rvz"
                | "wia"
                | "gcz"
                | "wbfs"
                | "nkit"
                | "pkg"
                | "cia"
                | "nsp"
                | "nsz"
                | "xcz"
                | "m3u"
                | "m3u8"
                | "gdi"
                | "ccd"
                | "mds"
        )
    }
}
