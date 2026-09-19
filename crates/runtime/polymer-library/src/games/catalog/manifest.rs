use super::{GameCatalog, GameCatalogError, Transaction, Uuid, Value};
use crate::games::PackageManifest;

impl GameCatalog {
    pub(super) fn decode_manifest(
        value: Value,
    ) -> Result<Option<PackageManifest>, GameCatalogError> {
        Self::optional_text(value)?
            .map(|text| {
                serde_json::from_str(&text)
                    .map_err(|error| GameCatalogError::InvalidData(error.to_string()))
            })
            .transpose()
    }

    pub(super) async fn write_manifest(
        transaction: &Transaction<'_>,
        id: Uuid,
        manifest: Option<&PackageManifest>,
    ) -> Result<(), GameCatalogError> {
        transaction
            .execute(
                "DELETE FROM launchable_manifests WHERE launchable_id = ?1",
                [Self::id(id)],
            )
            .await?;
        if let Some(manifest) = manifest {
            let json = serde_json::to_string(manifest)
                .map_err(|error| GameCatalogError::InvalidData(error.to_string()))?;
            transaction.execute(
                "INSERT INTO launchable_manifests (launchable_id, revision, manifest_json) VALUES (?1, ?2, ?3)",
                turso::params![Self::id(id), manifest.revision.clone(), json],
            ).await?;
        }
        Ok(())
    }
}
