use std::{io::Read, path::Path};

use object_store::{ObjectStoreExt, buffered::BufWriter, path::Path as ObjectPath};
use tokio::io::AsyncWriteExt;
use turso::{Value, transaction::Transaction};
use uuid::Uuid;

use super::{GameCatalog, GameCatalogError, PreparedObject};

impl GameCatalog {
    pub(super) async fn store_object(
        &self,
        source: &Path,
    ) -> Result<PreparedObject, GameCatalogError> {
        let source = source.to_owned();
        let hash_source = source.clone();
        let (digest, byte_length) = tokio::task::spawn_blocking(move || {
            let mut file = std::fs::File::open(&hash_source).map_err(|source| {
                GameCatalogError::MediaFile {
                    path: hash_source.clone(),
                    source,
                }
            })?;
            let mut hasher = blake3::Hasher::new();
            let mut buffer = [0_u8; 64 * 1024];
            let mut length = 0_u64;
            loop {
                let read =
                    file.read(&mut buffer)
                        .map_err(|source| GameCatalogError::MediaFile {
                            path: hash_source.clone(),
                            source,
                        })?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
                length = length.saturating_add(read as u64);
            }
            Ok::<_, GameCatalogError>((*hasher.finalize().as_bytes(), length))
        })
        .await
        .map_err(|error| {
            GameCatalogError::InvalidData(format!("media hashing task failed: {error}"))
        })??;
        let encoded = blake3::Hash::from_bytes(digest).to_hex().to_string();
        let key = ObjectPath::from(format!("library/media/{}/{encoded}", &encoded[..2]));
        match self.objects.head(&key).await {
            Ok(existing) if existing.size == byte_length => {}
            Ok(_) | Err(object_store::Error::NotFound { .. }) => {
                let mut input = tokio::fs::File::open(&source)
                    .await
                    .map_err(|source_error| GameCatalogError::MediaFile {
                        path: source.clone(),
                        source: source_error,
                    })?;
                let mut output = BufWriter::new(self.objects.clone(), key.clone());
                tokio::io::copy(&mut input, &mut output)
                    .await
                    .map_err(|source_error| GameCatalogError::MediaFile {
                        path: source.clone(),
                        source: source_error,
                    })?;
                output.shutdown().await?;
            }
            Err(error) => return Err(error.into()),
        }
        Ok(PreparedObject {
            digest,
            key,
            byte_length,
        })
    }

    pub(super) async fn reconcile_objects(&self) -> Result<(), GameCatalogError> {
        use futures_util::TryStreamExt;
        let connection = self.connection().await?;
        let mut rows = connection
            .query(
                r#"SELECT blake3, object_key FROM media_objects
                   WHERE NOT EXISTS (
                       SELECT 1 FROM metadata WHERE object_blake3 = media_objects.blake3
                   )"#,
                (),
            )
            .await?;
        let mut unreferenced = Vec::new();
        while let Some(row) = rows.next().await? {
            unreferenced.push((Self::blob(row.get_value(0)?)?, row.get::<String>(1)?));
        }
        drop(rows);
        for (digest, key) in unreferenced {
            match self.objects.delete(&ObjectPath::from(key)).await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => {
                    connection
                        .execute(
                            "DELETE FROM media_objects WHERE blake3 = ?1 AND NOT EXISTS (SELECT 1 FROM metadata WHERE object_blake3 = ?1)",
                            [Value::from(digest)],
                        )
                        .await?;
                }
                Err(error) => tracing::warn!(%error, "could not remove unreferenced library media"),
            }
        }
        let mut listing = self.objects.list(Some(&ObjectPath::from("library/media")));
        while let Some(object) = listing.try_next().await? {
            let mut rows = connection
                .query(
                    "SELECT 1 FROM media_objects WHERE object_key = ?1 LIMIT 1",
                    [Value::from(object.location.to_string())],
                )
                .await?;
            let referenced = rows.next().await?.is_some();
            drop(rows);
            if !referenced && let Err(error) = self.objects.delete(&object.location).await {
                tracing::warn!(%error, key = %object.location, "could not remove orphaned library media");
            }
        }
        Ok(())
    }

    pub(super) async fn collect_objects(&self, digests: Vec<[u8; 32]>) {
        for digest in digests {
            let connection = match self.connection().await {
                Ok(connection) => connection,
                Err(error) => {
                    tracing::warn!(%error, "could not inspect unreferenced library media");
                    return;
                }
            };
            let mut rows = match connection
                .query(
                    r#"SELECT media_objects.object_key
                       FROM media_objects
                       WHERE media_objects.blake3 = ?1
                         AND NOT EXISTS (SELECT 1 FROM metadata WHERE object_blake3 = ?1)"#,
                    [Value::from(digest.to_vec())],
                )
                .await
            {
                Ok(rows) => rows,
                Err(error) => {
                    tracing::warn!(%error, "could not inspect unreferenced library media");
                    continue;
                }
            };
            let key = match rows.next().await {
                Ok(Some(row)) => row.get::<String>(0).ok(),
                Ok(None) => None,
                Err(error) => {
                    tracing::warn!(%error, "could not inspect unreferenced library media");
                    None
                }
            };
            drop(rows);
            let Some(key) = key else { continue };
            match self.objects.delete(&ObjectPath::from(key.clone())).await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => {}
                Err(error) => {
                    tracing::warn!(%error, %key, "could not remove unreferenced library media");
                    continue;
                }
            }
            if let Err(error) = connection
                .execute(
                    "DELETE FROM media_objects WHERE blake3 = ?1 AND NOT EXISTS (SELECT 1 FROM metadata WHERE object_blake3 = ?1)",
                    [Value::from(digest.to_vec())],
                )
                .await
            {
                tracing::warn!(%error, %key, "could not remove library media index row");
            }
        }
    }

    pub(super) async fn referenced_objects(
        transaction: &Transaction<'_>,
        parent: Uuid,
        source: Option<&str>,
    ) -> Result<Vec<[u8; 32]>, GameCatalogError> {
        let (sql, params) = match source {
            Some(source) => (
                "SELECT DISTINCT object_blake3 FROM metadata WHERE parent_binary_id = ?1 AND source = ?2 AND object_blake3 IS NOT NULL",
                vec![Self::id(parent), Value::from(source)],
            ),
            None => (
                "SELECT DISTINCT object_blake3 FROM metadata WHERE parent_binary_id = ?1 AND object_blake3 IS NOT NULL",
                vec![Self::id(parent)],
            ),
        };
        let mut rows = transaction.query(sql, params).await?;
        let mut result = Vec::new();
        while let Some(row) = rows.next().await? {
            if let Some(digest) = Self::optional_digest(row.get_value(0)?)? {
                result.push(digest);
            }
        }
        Ok(result)
    }
}
