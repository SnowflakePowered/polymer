use super::*;
use std::collections::BTreeSet;

impl GameCatalog {
    pub async fn reconcile_storage(
        &self,
        manager: &crate::StorageManager,
    ) -> Result<(), GameCatalogError> {
        let connection = self.connection().await?;
        let mut rows = connection
            .query(
                "SELECT path FROM launchables WHERE path GLOB 'artifact://*'",
                (),
            )
            .await?;
        let mut volumes = BTreeSet::new();
        while let Some(row) = rows.next().await? {
            if let LaunchableUri::Artifact(artifact) = row.get::<String>(0)?.parse()? {
                volumes.insert((artifact.platform, artifact.storage));
            }
        }
        drop(rows);
        for (platform, id) in volumes {
            let info = manager
                .volume_info(&platform, id)
                .await
                .map_err(std::io::Error::other)?;
            let artifacts = if info.is_some() {
                let inspection = manager
                    .inspect(&platform, id)
                    .await
                    .map_err(std::io::Error::other)?;
                if inspection.dirty {
                    continue;
                }
                inspection.artifacts
            } else {
                Vec::new()
            };
            self.retain_artifacts(&platform, id, &artifacts).await?;
        }
        Ok(())
    }

    pub async fn retain_artifacts(
        &self,
        platform: &str,
        storage: Uuid,
        installed: &[crate::ArtifactSummary],
    ) -> Result<(), GameCatalogError> {
        let mut connection = self.connection().await?;
        let transaction = connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        let mut rows = transaction.query(
            "SELECT l.id, l.path, COALESCE(b.parent_binary_id, b.id) FROM launchables l JOIN binaries b ON b.id = l.binary_id WHERE l.path GLOB 'artifact://*'", ()
        ).await?;
        let mut missing = Vec::new();
        let mut roots = BTreeSet::new();
        while let Some(row) = rows.next().await? {
            let LaunchableUri::Artifact(artifact) = row.get::<String>(1)?.parse()? else {
                continue;
            };
            if artifact.platform == platform
                && artifact.storage == storage
                && !installed.iter().any(|item| item.id == artifact.artifact)
            {
                missing.push(Self::uuid(row.get_value(0)?)?);
                roots.insert(Self::uuid(row.get_value(2)?)?);
            }
        }
        drop(rows);
        for id in missing {
            transaction
                .execute("DELETE FROM launchables WHERE id = ?1", [Self::id(id)])
                .await?;
        }
        let mut objects = Vec::new();
        for root in roots {
            let mut rows = transaction.query(
                "SELECT COUNT(*) FROM launchables l JOIN binaries b ON b.id = l.binary_id WHERE b.id = ?1 OR b.parent_binary_id = ?1", [Self::id(root)]
            ).await?;
            let count: i64 = rows
                .next()
                .await?
                .ok_or_else(|| GameCatalogError::InvalidData("missing launchable count".into()))?
                .get(0)?;
            let empty = count == 0;
            drop(rows);
            if empty {
                objects.extend(Self::referenced_objects(&transaction, root, None).await?);
                transaction
                    .execute(
                        "DELETE FROM binaries WHERE parent_binary_id = ?1",
                        [Self::id(root)],
                    )
                    .await?;
                transaction
                    .execute("DELETE FROM binaries WHERE id = ?1", [Self::id(root)])
                    .await?;
            }
        }
        transaction.commit().await?;
        self.collect_objects(objects).await;
        Ok(())
    }
}
