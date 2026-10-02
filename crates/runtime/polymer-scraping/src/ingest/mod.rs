mod cue;
mod manifest;

use crate::{Package, ScrapingEngine, ScrapingError};
pub(crate) use cue::CuePreparation;
pub use manifest::{LaunchBundle, LaunchFile};
use polymer_library::{DiscSets, Launchable};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub struct Ingestor;

impl Ingestor {
    /// Only directories are enumerated. Individual files go unchanged to Gumshoe.
    pub fn identify(
        path: &Path,
        platform: Option<&str>,
        parent: Option<Uuid>,
    ) -> Result<Vec<Package>, ScrapingError> {
        let paths = Self::entrypoints(path)?;
        paths
            .into_iter()
            .map(|path| Package::identify(path, platform, parent))
            .collect()
    }

    pub(crate) fn entrypoints(path: &Path) -> Result<Vec<PathBuf>, ScrapingError> {
        if !path.is_dir() {
            return Ok(vec![path.to_owned()]);
        }
        let mut sheets = Vec::new();
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            if entry.file_type()?.is_file()
                && entry
                    .path()
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("cue"))
            {
                sheets.push(entry.path());
            }
        }
        sheets.sort();
        if sheets.is_empty() {
            sheets.push(path.to_owned());
        }
        Ok(sheets)
    }

    pub async fn import(
        engine: &ScrapingEngine,
        mut packages: Vec<Package>,
    ) -> Result<Vec<(Launchable, Option<polymer_library::jobs::Job>)>, ScrapingError> {
        packages.sort_by_key(|package| {
            DiscSets::get().disc(
                &package.platform,
                package.serial.as_deref(),
                package.disc_number,
            )
        });
        let mut imported = Vec::new();
        for package in packages {
            let (launchable, created) = package.import(engine.catalog()).await?;
            let root = launchable
                .identified
                .parent_binary_id
                .unwrap_or(launchable.binary_id);
            engine.publish_library_content(root);
            let job = if created {
                Some(engine.enqueue(root, launchable.binary_id).await?)
            } else {
                None
            };
            imported.push((launchable, job));
        }
        Ok(imported)
    }
}
