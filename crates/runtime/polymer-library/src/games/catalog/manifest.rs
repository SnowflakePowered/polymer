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

    /// Load preparation work without reading game metadata.
    pub async fn pending_manifests(
        &self,
        root: Option<Uuid>,
    ) -> Result<Vec<(Uuid, Uuid, PackageManifest)>, GameCatalogError> {
        let connection = self.connection().await?;
        let mut rows = connection.query(
            "SELECT m.launchable_id, COALESCE(b.parent_binary_id, b.id), m.manifest_json FROM launchable_manifests m JOIN launchables l ON l.id = m.launchable_id JOIN binaries b ON b.id = l.binary_id WHERE (?1 IS NULL OR COALESCE(b.parent_binary_id, b.id) = ?1)",
            [root.map(Self::id).unwrap_or(Value::Null)],
        ).await?;
        let mut pending = Vec::new();
        while let Some(row) = rows.next().await? {
            if let Some(manifest) = Self::decode_manifest(row.get_value(2)?)?
                && manifest.template.is_none()
            {
                pending.push((
                    Self::uuid(row.get_value(0)?)?,
                    Self::uuid(row.get_value(1)?)?,
                    manifest,
                ));
            }
        }
        Ok(pending)
    }

    /// Compare-and-swap prevents an older finalizer replacing a re-imported manifest.
    pub async fn finish_manifest(
        &self,
        id: Uuid,
        manifest: &PackageManifest,
    ) -> Result<(), GameCatalogError> {
        let json = serde_json::to_string(manifest)
            .map_err(|error| GameCatalogError::InvalidData(error.to_string()))?;
        self.connection().await?.execute(
            "UPDATE launchable_manifests SET manifest_json = ?1 WHERE launchable_id = ?2 AND revision = ?3",
            turso::params![json, Self::id(id), manifest.revision.clone()],
        ).await?;
        Ok(())
    }
}
