use std::path::PathBuf;

use turso::{Builder, Connection, Value, params};

use crate::error::Error;
use jiff::Timestamp;
use uuid::Uuid;

use super::BoxFuture;
use crate::installable::artifacts::Artifact;
use crate::installable::{DepotFile, DepotType, IconHint};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS nand (
    id       TEXT PRIMARY KEY,
    platform TEXT NOT NULL,
    label    TEXT,
    created  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS artifacts (
    id          TEXT PRIMARY KEY,
    depot_type  INTEGER NOT NULL,
    label       TEXT NOT NULL,
    version     TEXT,
    icon_path   TEXT,
    byte_length INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS artifact_files (
    artifact_id TEXT NOT NULL,
    path        TEXT NOT NULL,
    blake3      BLOB NOT NULL,
    byte_length INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS artifact_files_by_artifact ON artifact_files (artifact_id);
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactSummary {
    pub id: String,
    pub depot_type: DepotType,
    pub label: String,
    pub version: Option<String>,
    pub byte_length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NandMetadata {
    pub id: Uuid,
    pub platform: String,
    pub label: Option<String>,
    pub created: Timestamp,
}

pub trait ArtifactRegistry: Send + Sync {
    fn record<'a>(&'a self, artifact: &'a Artifact) -> BoxFuture<'a, Result<(), Error>>;
    fn summaries(&self) -> BoxFuture<'_, Result<Vec<ArtifactSummary>, Error>>;
    fn artifact<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<Artifact>, Error>>;
    fn contains<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<bool, Error>>;
    fn forget<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), Error>>;
}

pub struct ArtifactDb {
    connection: Connection,
}

impl ArtifactDb {
    pub async fn open(path: &str) -> Result<Self, Error> {
        let db = Builder::new_local(path).build().await.map_err(database)?;
        let connection = db.connect().map_err(database)?;
        connection.execute_batch(SCHEMA).await.map_err(database)?;

        Ok(Self { connection })
    }

    pub async fn open_in_memory() -> Result<Self, Error> {
        Self::open(":memory:").await
    }

    /// Record an installed artifact, replacing any earlier record of the same id.
    async fn insert(&self, artifact: &Artifact) -> Result<(), Error> {
        self.delete(artifact.id()).await?;

        let icon_path = match artifact.icon() {
            IconHint::File(path) => Some(path.to_string_lossy().into_owned()),
            IconHint::Builtin(_) => None,
        };

        self.connection
            .execute(
                "INSERT INTO artifacts (id, depot_type, label, version, icon_path, byte_length)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    artifact.id(),
                    i64::from(u8::from(artifact.depot_type())),
                    artifact.label(),
                    artifact.version(),
                    icon_path,
                    artifact.byte_length() as i64,
                ],
            )
            .await
            .map_err(database)?;

        for file in artifact.manifest() {
            self.connection
                .execute(
                    "INSERT INTO artifact_files (artifact_id, path, blake3, byte_length)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        artifact.id(),
                        file.path().to_string_lossy().into_owned(),
                        file.blake3().to_vec(),
                        file.byte_length() as i64,
                    ],
                )
                .await
                .map_err(database)?;
        }

        Ok(())
    }

    async fn list(&self) -> Result<Vec<ArtifactSummary>, Error> {
        let mut rows = self
            .connection
            .query(
                "SELECT id, depot_type, label, version, byte_length
                 FROM artifacts ORDER BY id",
                (),
            )
            .await
            .map_err(database)?;

        let mut summaries = Vec::new();
        while let Some(row) = rows.next().await.map_err(database)? {
            summaries.push(ArtifactSummary {
                id: text(&row, 0)?,
                depot_type: depot_type_from(integer(&row, 1)?)?,
                label: text(&row, 2)?,
                version: optional_text(&row, 3)?,
                byte_length: integer(&row, 4)? as u64,
            });
        }

        Ok(summaries)
    }

    async fn has(&self, id: &str) -> Result<bool, Error> {
        let mut rows = self
            .connection
            .query("SELECT 1 FROM artifacts WHERE id = ?1", params![id])
            .await
            .map_err(database)?;

        Ok(rows.next().await.map_err(database)?.is_some())
    }

    async fn manifest(&self, id: &str) -> Result<Vec<DepotFile>, Error> {
        let mut rows = self
            .connection
            .query(
                "SELECT path, blake3, byte_length FROM artifact_files
                 WHERE artifact_id = ?1 ORDER BY path",
                params![id],
            )
            .await
            .map_err(database)?;

        let mut files = Vec::new();
        while let Some(row) = rows.next().await.map_err(database)? {
            let path = text(&row, 0)?;
            let digest: [u8; 32] = blob(&row, 1)?.try_into().map_err(|_| Error::Database {
                reason: format!("`{path}` has a malformed digest"),
            })?;

            files.push(DepotFile::new(
                PathBuf::from(path),
                digest,
                integer(&row, 2)? as u64,
            ));
        }

        Ok(files)
    }

    async fn read_identity(&self) -> Result<Option<NandMetadata>, Error> {
        let mut rows = self
            .connection
            .query("SELECT id, platform, label, created FROM nand LIMIT 1", ())
            .await
            .map_err(database)?;

        let Some(row) = rows.next().await.map_err(database)? else {
            return Ok(None);
        };

        let id = text(&row, 0)?;
        let id = Uuid::parse_str(&id).map_err(|_| Error::Database {
            reason: format!("`{id}` is not a uuid"),
        })?;
        let created = integer(&row, 3)?;
        let created = Timestamp::from_second(created).map_err(|_| Error::Database {
            reason: format!("`{created}` is not a timestamp"),
        })?;

        Ok(Some(NandMetadata {
            id,
            platform: text(&row, 1)?,
            label: optional_text(&row, 2)?,
            created,
        }))
    }

    /// Establish an identity if there is none, returning it either way. An
    /// already-branded manifest keeps its id, platform, and label.
    async fn claim_identity(
        &self,
        id: Uuid,
        platform: &str,
        label: Option<&str>,
    ) -> Result<NandMetadata, Error> {
        if let Some(identity) = self.read_identity().await? {
            return Ok(identity);
        }

        let identity = NandMetadata {
            id,
            platform: platform.to_string(),
            label: label.map(str::to_string),
            created: Timestamp::now(),
        };

        self.connection
            .execute(
                "INSERT INTO nand (id, platform, label, created) VALUES (?1, ?2, ?3, ?4)",
                params![
                    identity.id.simple().to_string(),
                    platform,
                    label,
                    identity.created.as_second(),
                ],
            )
            .await
            .map_err(database)?;

        Ok(identity)
    }

    async fn write_label(&self, label: &str) -> Result<(), Error> {
        let changed = self
            .connection
            .execute("UPDATE nand SET label = ?1", params![label])
            .await
            .map_err(database)?;

        match changed {
            0 => Err(Error::UnclaimedNand),
            _ => Ok(()),
        }
    }

    /// Rebuild the artifact `id` was recorded from.
    async fn fetch(&self, id: &str) -> Result<Option<Artifact>, Error> {
        let mut rows = self
            .connection
            .query(
                "SELECT depot_type, label, version, icon_path FROM artifacts WHERE id = ?1",
                params![id],
            )
            .await
            .map_err(database)?;

        let Some(row) = rows.next().await.map_err(database)? else {
            return Ok(None);
        };

        let depot_type = depot_type_from(integer(&row, 0)?)?;
        let label = text(&row, 1)?;
        let version = optional_text(&row, 2)?;
        let icon = match optional_text(&row, 3)? {
            Some(path) => IconHint::File(PathBuf::from(path)),
            None => IconHint::Builtin(depot_type),
        };

        Ok(Some(Artifact::new(
            id.to_string(),
            depot_type,
            self.manifest(id).await?,
            label,
            version,
            icon,
        )))
    }

    async fn delete(&self, id: &str) -> Result<(), Error> {
        self.connection
            .execute("DELETE FROM artifact_files WHERE artifact_id = ?1", params![id])
            .await
            .map_err(database)?;
        self.connection
            .execute("DELETE FROM artifacts WHERE id = ?1", params![id])
            .await
            .map_err(database)?;

        Ok(())
    }

    pub(super) fn brand<'a>(
        &'a self,
        id: Uuid,
        platform: &'a str,
        label: Option<&'a str>,
    ) -> BoxFuture<'a, Result<NandMetadata, Error>> {
        Box::pin(self.claim_identity(id, platform, label))
    }

    pub(super) fn identity(&self) -> BoxFuture<'_, Result<Option<NandMetadata>, Error>> {
        Box::pin(self.read_identity())
    }

    pub(super) fn set_label<'a>(&'a self, label: &'a str) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(self.write_label(label))
    }
}

impl ArtifactRegistry for ArtifactDb {

    fn record<'a>(&'a self, artifact: &'a Artifact) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(self.insert(artifact))
    }

    fn summaries(&self) -> BoxFuture<'_, Result<Vec<ArtifactSummary>, Error>> {
        Box::pin(self.list())
    }

    fn artifact<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<Artifact>, Error>> {
        Box::pin(self.fetch(id))
    }

    fn contains<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<bool, Error>> {
        Box::pin(self.has(id))
    }

    fn forget<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(self.delete(id))
    }
}

fn depot_type_from(id: i64) -> Result<DepotType, Error> {
    u8::try_from(id)
        .ok()
        .and_then(|id| DepotType::try_from(id).ok())
        .ok_or_else(|| Error::Database {
            reason: format!("unknown depot type `{id}`"),
        })
}

fn text(row: &turso::Row, index: usize) -> Result<String, Error> {
    match row.get_value(index).map_err(database)? {
        Value::Text(text) => Ok(text),
        other => Err(unexpected(index, "text", &other)),
    }
}

fn optional_text(row: &turso::Row, index: usize) -> Result<Option<String>, Error> {
    match row.get_value(index).map_err(database)? {
        Value::Text(text) => Ok(Some(text)),
        Value::Null => Ok(None),
        other => Err(unexpected(index, "text or null", &other)),
    }
}

fn integer(row: &turso::Row, index: usize) -> Result<i64, Error> {
    match row.get_value(index).map_err(database)? {
        Value::Integer(value) => Ok(value),
        other => Err(unexpected(index, "integer", &other)),
    }
}

fn blob(row: &turso::Row, index: usize) -> Result<Vec<u8>, Error> {
    match row.get_value(index).map_err(database)? {
        Value::Blob(bytes) => Ok(bytes),
        other => Err(unexpected(index, "blob", &other)),
    }
}

fn unexpected(index: usize, wanted: &str, found: &Value) -> Error {
    Error::Database {
        reason: format!("column {index} is {found:?}, expected {wanted}"),
    }
}

fn database(error: turso::Error) -> Error {
    Error::Database {
        reason: error.to_string(),
    }
}
