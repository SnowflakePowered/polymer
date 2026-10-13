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
                "SELECT path, install_path IS NULL FROM launchables WHERE path GLOB 'artifact://*'",
                (),
            )
            .await?;
        let mut volumes = BTreeSet::new();
        let mut unplaced = Vec::new();
        while let Some(row) = rows.next().await? {
            let path: String = row.get(0)?;
            if let LaunchableUri::Artifact(artifact) = path.parse()? {
                if row.get::<i64>(1)? != 0 {
                    unplaced.push((path, artifact.clone()));
                }
                volumes.insert((artifact.platform, artifact.storage));
            }
        }
        drop(rows);
        for (path, artifact) in unplaced {
            let Ok((_, entrypoint)) = manager.artifact_entrypoint(
                &artifact.platform,
                artifact.storage,
                &artifact.artifact,
            ) else {
                continue;
            };
            connection
                .execute(
                    "UPDATE launchables SET install_path = ?2 WHERE path = ?1",
                    vec![Value::from(path), Value::from(entrypoint.root)],
                )
                .await?;
        }
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
        let objects = Self::prune_roots(&transaction, &roots).await?;
        transaction.commit().await?;
        self.collect_objects(objects).await;
        Ok(())
    }
}
