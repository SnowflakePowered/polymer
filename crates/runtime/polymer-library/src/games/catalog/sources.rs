use super::*;
use polymer_consolestorage::installable::{DepotType, InPlaceTitle};
use std::collections::BTreeSet;
use std::path::Path;

/// A watched folder whose games and in-place titles are imported automatically.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibrarySource {
    pub id: Uuid,
    pub root: PathBuf,
    pub created_at: i64,
}

/// Who imported a launchable. Only source imports are removed with their source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportOrigin {
    Manual,
    Source,
}

/// A host title directory recorded for mounting into the managed NAND.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MountedTitleRow {
    pub host: PathBuf,
    pub install_path: String,
}

const UNDER: &str = "(?1 = root_key OR (length(?1) > length(root_key) AND substr(?1, 1, length(root_key) + 1) = root_key || '/'))";

impl LibrarySource {
    /// The comparable form of a host path: absolute, `/`-separated, without a
    /// trailing separator, and case-folded where the host filesystem is.
    pub fn key(path: &Path) -> String {
        let path = dunce::simplified(path).to_string_lossy().replace('\\', "/");
        let path = path.trim_end_matches('/');
        if cfg!(any(windows, target_os = "macos")) {
            path.to_lowercase()
        } else {
            path.to_owned()
        }
    }

    pub fn covers(&self, path: &Path) -> bool {
        let (root, path) = (Self::key(&self.root), Self::key(path));
        path == root
            || path
                .strip_prefix(&root)
                .is_some_and(|rest| rest.starts_with('/'))
    }
}

impl GameCatalog {
    /// Whether an import at `host_key` may be committed by a source: an active
    /// source covers it, or it was already imported manually.
    pub(super) async fn covered(
        transaction: &Transaction<'_>,
        host_key: Option<&str>,
    ) -> Result<bool, GameCatalogError> {
        let Some(host_key) = host_key else {
            return Ok(false);
        };
        let mut rows = transaction
            .query(
                &format!(
                    "SELECT EXISTS (SELECT 1 FROM library_sources WHERE {UNDER})
                         OR EXISTS (SELECT 1 FROM launchables WHERE host_key = ?1 AND manual = 1)"
                ),
                [Value::from(host_key.to_owned())],
            )
            .await?;
        let covered: i64 = rows
            .next()
            .await?
            .ok_or_else(|| GameCatalogError::InvalidData("missing coverage".into()))?
            .get(0)?;
        Ok(covered != 0)
    }

    pub async fn sources(&self) -> Result<Vec<LibrarySource>, GameCatalogError> {
        let connection = self.connection().await?;
        let mut rows = connection
            .query(
                "SELECT id, root, created_at FROM library_sources ORDER BY root_key",
                (),
            )
            .await?;
        let mut sources = Vec::new();
        while let Some(row) = rows.next().await? {
            sources.push(LibrarySource {
                id: Self::uuid(row.get_value(0)?)?,
                root: PathBuf::from(row.get::<String>(1)?),
                created_at: row.get(2)?,
            });
        }
        Ok(sources)
    }

    pub async fn add_source(&self, root: &Path) -> Result<LibrarySource, GameCatalogError> {
        if !root.is_absolute() {
            return Err(GameCatalogError::InvalidData(
                "library sources must be absolute".to_owned(),
            ));
        }
        let source = LibrarySource {
            id: Uuid::now_v7(),
            root: dunce::simplified(root).to_owned(),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_millis() as i64),
        };
        let connection = self.connection().await?;
        connection
            .execute(
                "INSERT INTO library_sources (id, root, root_key, created_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(root_key) DO NOTHING",
                vec![
                    Self::id(source.id),
                    Value::from(source.root.to_string_lossy().into_owned()),
                    Value::from(LibrarySource::key(&source.root)),
                    Value::from(source.created_at),
                ],
            )
            .await?;
        self.sources()
            .await?
            .into_iter()
            .find(|existing| LibrarySource::key(&existing.root) == LibrarySource::key(root))
            .ok_or_else(|| GameCatalogError::NotFound(root.display().to_string()))
    }

    /// Removes a source and, in the same transaction, everything it imported
    /// that no other source covers. Returns the games that changed.
    pub async fn remove_source(&self, id: Uuid) -> Result<Vec<Uuid>, GameCatalogError> {
        let mut connection = self.connection().await?;
        let transaction = connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        let mut rows = transaction
            .query(
                "SELECT root_key FROM library_sources WHERE id = ?1",
                [Self::id(id)],
            )
            .await?;
        let key: String = rows
            .next()
            .await?
            .ok_or_else(|| GameCatalogError::NotFound(id.simple().to_string()))?
            .get(0)?;
        drop(rows);
        transaction
            .execute("DELETE FROM library_sources WHERE id = ?1", [Self::id(id)])
            .await?;
        let uncovered = format!(
            "(host_key = ?1 OR substr(host_key, 1, length(?1) + 1) = ?1 || '/')
             AND NOT EXISTS (SELECT 1 FROM library_sources WHERE {})",
            UNDER.replace("?1", "host_key")
        );
        transaction
            .execute(
                &format!("DELETE FROM mounted_titles WHERE {uncovered}"),
                [Value::from(key.clone())],
            )
            .await?;
        let roots = Self::delete_launchables(
            &transaction,
            &format!("manual = 0 AND {uncovered}"),
            vec![Value::from(key)],
        )
        .await?;
        let objects = Self::prune_roots(&transaction, &roots).await?;
        transaction.commit().await?;
        self.collect_objects(objects).await;
        Ok(roots.into_iter().collect())
    }

    /// Host paths a source imported under `root`, launchables and mounted titles alike.
    pub async fn source_paths(&self, root: &Path) -> Result<Vec<PathBuf>, GameCatalogError> {
        let connection = self.connection().await?;
        let key = Value::from(LibrarySource::key(root));
        let under = "(host_key = ?1 OR substr(host_key, 1, length(?1) + 1) = ?1 || '/')";
        let mut paths = Vec::new();
        let mut rows = connection
            .query(
                &format!("SELECT path FROM launchables WHERE manual = 0 AND {under}"),
                [key.clone()],
            )
            .await?;
        while let Some(row) = rows.next().await? {
            if let Some(host) = row
                .get::<String>(0)?
                .parse::<LaunchableUri>()?
                .source_path()
            {
                paths.push(host.to_owned());
            }
        }
        drop(rows);
        let mut rows = connection
            .query(
                &format!("SELECT host FROM mounted_titles WHERE {under}"),
                [key],
            )
            .await?;
        while let Some(row) = rows.next().await? {
            paths.push(PathBuf::from(row.get::<String>(0)?));
        }
        Ok(paths)
    }

    /// Drops what a source imported at exactly `host`, after it disappeared or
    /// stopped being an importable unit. Manual imports are kept.
    pub async fn forget(&self, host: &Path) -> Result<Vec<Uuid>, GameCatalogError> {
        let key = LibrarySource::key(host);
        let mut connection = self.connection().await?;
        let transaction = connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        transaction
            .execute(
                "DELETE FROM mounted_titles WHERE host_key = ?1",
                [Value::from(key.clone())],
            )
            .await?;
        let roots = Self::delete_launchables(
            &transaction,
            "manual = 0 AND host_key = ?1",
            vec![Value::from(key)],
        )
        .await?;
        let objects = Self::prune_roots(&transaction, &roots).await?;
        transaction.commit().await?;
        self.collect_objects(objects).await;
        Ok(roots.into_iter().collect())
    }

    /// Whether the launchable at `host` was imported from a file whose size and
    /// modification time are still `stamp`.
    pub async fn stamped(&self, host: &Path, stamp: &str) -> Result<bool, GameCatalogError> {
        let connection = self.connection().await?;
        let mut rows = connection
            .query(
                "SELECT 1 FROM launchables WHERE host_key = ?1 AND host_stamp = ?2",
                vec![
                    Value::from(LibrarySource::key(host)),
                    Value::from(stamp.to_owned()),
                ],
            )
            .await?;
        Ok(rows.next().await?.is_some())
    }

    pub async fn stamp(&self, host: &Path, stamp: &str) -> Result<(), GameCatalogError> {
        self.connection()
            .await?
            .execute(
                "UPDATE launchables SET host_stamp = ?2 WHERE host_key = ?1",
                vec![
                    Value::from(LibrarySource::key(host)),
                    Value::from(stamp.to_owned()),
                ],
            )
            .await?;
        Ok(())
    }

    /// Records an in-place title found by a source, if a source still covers it.
    pub async fn mount_title(
        &self,
        host: &Path,
        title: &InPlaceTitle,
    ) -> Result<(), GameCatalogError> {
        let key = LibrarySource::key(host);
        let mut connection = self.connection().await?;
        let transaction = connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        if !Self::covered(&transaction, Some(&key)).await? {
            return Err(GameCatalogError::Uncovered(host.display().to_string()));
        }
        let kind = match title.kind {
            DepotType::App => "app",
            DepotType::AppContent => "appcontent",
            DepotType::AppUpdate => "appupdate",
            DepotType::System => {
                return Err(GameCatalogError::InvalidData(
                    "system titles are not mounted".to_owned(),
                ));
            }
        };
        let mut rows = transaction
            .query(
                "SELECT host FROM mounted_titles WHERE platform = ?1 AND install_path = ?2 AND host_key != ?3",
                vec![
                    Value::from(title.platform.clone()),
                    Value::from(title.install_path.clone()),
                    Value::from(key.clone()),
                ],
            )
            .await?;
        if let Some(row) = rows.next().await? {
            return Err(GameCatalogError::MountedElsewhere(PathBuf::from(
                row.get::<String>(0)?,
            )));
        }
        drop(rows);
        transaction
            .execute(
                "INSERT INTO mounted_titles (host_key, host, platform, title, kind, install_path)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(host_key) DO UPDATE SET host = excluded.host, platform = excluded.platform,
                     title = excluded.title, kind = excluded.kind, install_path = excluded.install_path",
                vec![
                    Value::from(key),
                    Value::from(host.to_string_lossy().into_owned()),
                    Value::from(title.platform.clone()),
                    Value::from(title.title.clone()),
                    Value::from(kind),
                    Value::from(title.install_path.clone()),
                ],
            )
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    /// The in-place components of a title: its base, updates, and DLC.
    pub async fn mounted_titles(
        &self,
        platform: &str,
        title: &str,
    ) -> Result<Vec<MountedTitleRow>, GameCatalogError> {
        let connection = self.connection().await?;
        let mut rows = connection
            .query(
                "SELECT host, install_path FROM mounted_titles WHERE platform = ?1 AND title = ?2",
                vec![
                    Value::from(platform.to_owned()),
                    Value::from(title.to_owned()),
                ],
            )
            .await?;
        let mut titles = Vec::new();
        while let Some(row) = rows.next().await? {
            titles.push(MountedTitleRow {
                host: PathBuf::from(row.get::<String>(0)?),
                install_path: row.get(1)?,
            });
        }
        Ok(titles)
    }

    /// Deletes matching launchables, returning the games they belonged to.
    async fn delete_launchables(
        transaction: &Transaction<'_>,
        condition: &str,
        parameters: Vec<Value>,
    ) -> Result<BTreeSet<Uuid>, GameCatalogError> {
        let mut rows = transaction
            .query(
                &format!(
                    "SELECT COALESCE(b.parent_binary_id, b.id) FROM launchables
                     JOIN binaries b ON b.id = launchables.binary_id WHERE {condition}"
                ),
                parameters.clone(),
            )
            .await?;
        let mut roots = BTreeSet::new();
        while let Some(row) = rows.next().await? {
            roots.insert(Self::uuid(row.get_value(0)?)?);
        }
        drop(rows);
        transaction
            .execute(
                &format!("DELETE FROM launchables WHERE {condition}"),
                parameters,
            )
            .await?;
        Ok(roots)
    }

    /// Deletes games left without any launchable, returning their media objects.
    pub(super) async fn prune_roots(
        transaction: &Transaction<'_>,
        roots: &BTreeSet<Uuid>,
    ) -> Result<Vec<[u8; 32]>, GameCatalogError> {
        let mut objects = Vec::new();
        for root in roots {
            let mut rows = transaction.query(
                "SELECT COUNT(*) FROM launchables l JOIN binaries b ON b.id = l.binary_id WHERE b.id = ?1 OR b.parent_binary_id = ?1", [Self::id(*root)]
            ).await?;
            let count: i64 = rows
                .next()
                .await?
                .ok_or_else(|| GameCatalogError::InvalidData("missing launchable count".into()))?
                .get(0)?;
            drop(rows);
            if count == 0 {
                objects.extend(Self::referenced_objects(transaction, *root, None).await?);
                transaction
                    .execute(
                        "DELETE FROM binaries WHERE parent_binary_id = ?1",
                        [Self::id(*root)],
                    )
                    .await?;
                transaction
                    .execute("DELETE FROM binaries WHERE id = ?1", [Self::id(*root)])
                    .await?;
            }
        }
        Ok(objects)
    }
}
