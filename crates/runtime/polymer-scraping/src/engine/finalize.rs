use super::{ScrapingEngine, ScrapingError};
use crate::ingest::CuePreparation;
use uuid::Uuid;

impl ScrapingEngine {
    /// Durable manifests also recover imports interrupted before a scrape was queued.
    pub async fn finalize_packages(&self, root: Option<Uuid>) -> Result<(), ScrapingError> {
        let pending = self.inner.catalog.pending_manifests(root).await?;
        let mut changed = std::collections::BTreeSet::new();
        for (id, root, mut manifest) in pending {
            let manifest = tokio::task::spawn_blocking(move || {
                if let Err(error) = CuePreparation::finalize(&mut manifest) {
                    manifest.error = Some(error.to_string());
                }
                manifest
            })
            .await
            .map_err(|error| ScrapingError::InvalidMedia(error.to_string()))?;
            self.inner.catalog.finish_manifest(id, &manifest).await?;
            changed.insert(root);
        }
        for root in changed {
            self.publish_library_content(root);
        }
        Ok(())
    }
}
