use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use object_store::{ObjectStore, path::Path as ObjectPath};
use turso::{Value, transaction::Transaction};
use uuid::Uuid;

use super::{
    Game, GamePage, GameQuery, GameSort, IdentifiedLaunchable, Launchable, LaunchableUri,
    MediaData, MediaPreview, MediaResource, Metadata, MetadataData, MetadataInput,
    MetadataInputData, MetadataValue, schema::SCHEMA,
};
use crate::Directories;

mod artifacts;
mod configuration;
mod identity;
mod manifest;
mod metadata;
mod objects;
mod sources;
mod titles;
mod updates;

pub use sources::{ImportOrigin, LibrarySource, MountedTitleRow};
use titles::TitleIndex;

#[derive(Debug, thiserror::Error)]
pub enum GameCatalogError {
    #[error("could not create the library directory: {0}")]
    CreateDirectory(#[source] std::io::Error),
    #[error("the library database path is not valid UTF-8")]
    InvalidDatabasePath,
    #[error("library database error: {0}")]
    Database(#[from] turso::Error),
    #[error("library object storage error: {0}")]
    ObjectStorage(#[from] object_store::Error),
    #[error("library I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("library media file error for {path}: {source}")]
    MediaFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid library data: {0}")]
    InvalidData(String),
    #[error("library record was not found: {0}")]
    NotFound(String),
    #[error("import disc 1 of {set} before disc {disc}")]
    MissingFirstDisc { set: String, disc: u8 },
    #[error("{0} is not in a library source")]
    Uncovered(String),
    #[error("this title is already mounted from {}", .0.display())]
    MountedElsewhere(PathBuf),
}

#[derive(Clone)]
pub struct GameCatalog {
    database: turso::Database,
    objects: Arc<dyn ObjectStore>,
    titles: Arc<TitleIndex>,
}

struct PreparedMetadata {
    key: String,
    source: String,
    producer: String,
    ordinal: u32,
    data: PreparedMetadataData,
}

enum PreparedMetadataData {
    Scalar(MetadataValue),
    Inline {
        media_type: String,
        bytes: Box<[u8]>,
        source_url: Option<String>,
        preview: Option<MediaPreview>,
    },
    Object {
        media_type: String,
        object: PreparedObject,
        source_url: Option<String>,
        preview: Option<MediaPreview>,
    },
}

struct PreparedObject {
    digest: [u8; 32],
    key: ObjectPath,
    byte_length: u64,
}

impl GameCatalog {
    pub async fn open(
        database_path: impl AsRef<Path>,
        objects: Arc<dyn ObjectStore>,
    ) -> Result<Self, GameCatalogError> {
        let database_path = database_path.as_ref();
        if let Some(parent) = database_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(GameCatalogError::CreateDirectory)?;
        }
        let path = database_path
            .to_str()
            .ok_or(GameCatalogError::InvalidDatabasePath)?;
        let database = turso::Builder::new_local(path).build().await?;
        let store = Self {
            database,
            objects,
            titles: Arc::default(),
        };
        store.initialize().await?;
        store.reconcile_objects().await?;
        store.sync_titles(true).await?;
        Ok(store)
    }

    pub async fn open_local(directories: &Directories) -> Result<Self, GameCatalogError> {
        let object_root = directories.object_storage();
        tokio::fs::create_dir_all(&object_root)
            .await
            .map_err(GameCatalogError::CreateDirectory)?;
        let objects = object_store::local::LocalFileSystem::new_with_prefix(&object_root)?;
        Self::open(directories.library_database(), Arc::new(objects)).await
    }

    pub fn objects(&self) -> &dyn ObjectStore {
        self.objects.as_ref()
    }

    pub async fn games(&self) -> Result<Vec<Game>, GameCatalogError> {
        self.load_games(None, None).await
    }

    pub async fn games_by_ids(
        &self,
        roots: &[Uuid],
        metadata_keys: Option<&[String]>,
    ) -> Result<Vec<Game>, GameCatalogError> {
        if roots.is_empty() {
            return Ok(Vec::new());
        }
        let mut games = self
            .load_games(Some(roots), metadata_keys)
            .await?
            .into_iter()
            .map(|game| (game.id, game))
            .collect::<BTreeMap<_, _>>();
        Ok(roots.iter().filter_map(|root| games.remove(root)).collect())
    }

    pub async fn game(&self, id: Uuid) -> Result<Option<Game>, GameCatalogError> {
        Ok(self.games_by_ids(&[id], None).await?.pop())
    }

    async fn load_games(
        &self,
        roots: Option<&[Uuid]>,
        metadata_keys: Option<&[String]>,
    ) -> Result<Vec<Game>, GameCatalogError> {
        self.sync_titles(false).await?;
        let (scope, parameters) = match roots {
            Some(roots) => {
                let list = (1..=roots.len())
                    .map(|index| format!("?{index}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                (
                    format!(
                        "IN (SELECT id FROM binaries WHERE id IN ({list}) UNION ALL SELECT id FROM binaries WHERE parent_binary_id IN ({list}))"
                    ),
                    roots.iter().copied().map(Self::id).collect::<Vec<_>>(),
                )
            }
            None => ("IS NOT NULL".to_owned(), Vec::new()),
        };
        let connection = self.connection().await?;
        let mut rows = connection
            .query(
                &format!(
                    r#"
                SELECT COALESCE(binary.parent_binary_id, binary.id), root.platform,
                       launchable.id, binary.id, launchable.path, binary.blake3,
                       binary.serial, binary.title_id, binary.parent_binary_id, manifest.manifest_json,
                       launchable.entrypoint, launchable.package, COALESCE(titles.title, ''),
                       binary.disc_number, binary.requires_license
                FROM launchables AS launchable
                LEFT JOIN launchable_manifests AS manifest ON manifest.launchable_id = launchable.id
                JOIN binaries AS binary ON binary.id = launchable.binary_id
                JOIN binaries AS root ON root.id = COALESCE(binary.parent_binary_id, binary.id)
                LEFT JOIN game_titles AS titles ON titles.root_binary_id = root.id
                WHERE binary.id {scope}
                  AND NOT (launchable.path GLOB 'mount://*' AND EXISTS (
                      SELECT 1 FROM launchables AS managed
                      JOIN binaries AS managed_binary ON managed_binary.id = managed.binary_id
                      WHERE managed.path GLOB 'artifact://*'
                        AND managed.install_path = launchable.install_path
                        AND managed_binary.platform = binary.platform))
                ORDER BY COALESCE(binary.parent_binary_id, binary.id),
                         binary.parent_binary_id IS NOT NULL, binary.id, launchable.path
                "#
                ),
                parameters.clone(),
            )
            .await?;
        let mut games = BTreeMap::<Uuid, Game>::new();
        let mut binary_roots = BTreeMap::<Uuid, Uuid>::new();
        while let Some(row) = rows.next().await? {
            let root_id = Self::uuid(row.get_value(0)?)?;
            let root_platform: String = row.get(1)?;
            let game = match games.entry(root_id) {
                std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::btree_map::Entry::Vacant(entry) => entry.insert(Game {
                    id: root_id,
                    platform: root_platform.clone(),
                    title: row.get(12)?,
                    launchables: Vec::new(),
                    metadata: Vec::new(),
                    updates: BTreeMap::new(),
                }),
            };
            let binary_id = Self::uuid(row.get_value(3)?)?;
            binary_roots.insert(binary_id, root_id);
            game.launchables.push(Launchable {
                id: Self::uuid(row.get_value(2)?)?,
                binary_id,
                identified: IdentifiedLaunchable {
                    manifest: Self::decode_manifest(row.get_value(9)?)?,
                    path: row.get::<String>(4)?.parse()?,
                    entrypoint: Self::optional_text(row.get_value(10)?)?,
                    package: Self::optional_text(row.get_value(11)?)?,
                    binary_blake3: Self::blob(row.get_value(5)?)?.try_into().map_err(|_| {
                        GameCatalogError::InvalidData("invalid binary digest".to_owned())
                    })?,
                    serial: Self::optional_text(row.get_value(6)?)?,
                    title_id: Self::optional_text(row.get_value(7)?)?,
                    disc_number: Self::optional_disc(row.get_value(13)?)?,
                    requires_license: row.get::<i64>(14)? != 0,
                    parent_binary_id: Self::optional_uuid(row.get_value(8)?)?,
                    platform: root_platform,
                },
            });
        }
        drop(rows);
        for game in games.values_mut() {
            game.launchables.sort_by_key(|launchable| {
                let identified = &launchable.identified;
                (
                    identified.parent_binary_id.is_some(),
                    identified.disc().unwrap_or(u8::MAX),
                )
            });
        }

        let (key_filter, metadata_parameters) = match metadata_keys {
            Some([]) => ("AND 0".to_owned(), parameters.clone()),
            Some(keys) => (
                format!(
                    "AND metadata.key IN ({})",
                    (parameters.len() + 1..=parameters.len() + keys.len())
                        .map(|index| format!("?{index}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                parameters
                    .iter()
                    .cloned()
                    .chain(keys.iter().cloned().map(Value::from))
                    .collect(),
            ),
            None => (String::new(), parameters.clone()),
        };
        let mut rows = connection
            .query(
                &format!(
                    r#"
                SELECT metadata.id, metadata.parent_binary_id, metadata.key,
                       metadata.source, metadata.producer, metadata.ordinal,
                       metadata.data_kind, metadata.string_value, metadata.number_value,
                       metadata.boolean_value, metadata.inline_data,
                       media_objects.object_key, metadata.media_type, metadata.source_url,
                       metadata.width, metadata.height, metadata.blurhash
                FROM metadata
                LEFT JOIN media_objects ON media_objects.blake3 = metadata.object_blake3
                WHERE metadata.parent_binary_id {scope} {key_filter}
                ORDER BY metadata.parent_binary_id, metadata.key,
                         metadata.source, metadata.ordinal, metadata.id
                "#
                ),
                metadata_parameters,
            )
            .await?;
        while let Some(row) = rows.next().await? {
            let parent = Self::uuid(row.get_value(1)?)?;
            if let Some(game) = binary_roots
                .get(&parent)
                .and_then(|root| games.get_mut(root))
            {
                game.metadata.push(Self::metadata_from_row(&row)?);
            }
        }
        drop(rows);
        let mut updates = Self::updates(&connection, &scope, parameters).await?;
        for game in games.values_mut() {
            for launchable in &game.launchables {
                if let Some(candidates) = updates.remove(&launchable.id) {
                    game.updates.insert(launchable.id, candidates);
                }
            }
        }
        Ok(games.into_values().collect())
    }

    /// Imports a launchable and replaces its host-produced seed metadata in one transaction.
    /// The boolean is true when this created a new binary, including a child disc.
    pub async fn import_package(
        &self,
        mut input: IdentifiedLaunchable,
        seeds: Vec<(String, Vec<MetadataInput>)>,
        origin: ImportOrigin,
    ) -> Result<(Launchable, bool), GameCatalogError> {
        if input.platform.is_empty() {
            return Err(GameCatalogError::InvalidData(
                "a launchable requires a platform".to_owned(),
            ));
        }
        let mut path = input.path.url()?.to_string();
        let host_key = input.path.source_path().map(LibrarySource::key);
        let install_path = input.path.install_path().map(str::to_owned);
        let mut prepared_seeds = Vec::with_capacity(seeds.len());
        for (producer, inputs) in seeds {
            let mut prepared = Vec::with_capacity(inputs.len());
            for (ordinal, input) in inputs.into_iter().enumerate() {
                let ordinal = u32::try_from(ordinal).map_err(|_| {
                    GameCatalogError::InvalidData("too many metadata values".to_owned())
                })?;
                prepared.push(self.prepare_metadata(&producer, ordinal, input).await?);
            }
            prepared_seeds.push((producer, prepared));
        }
        let binary_id = Self::binary_id(
            &input.binary_blake3,
            &input.platform,
            input.serial.as_deref(),
            input.disc_number,
        );
        let mut connection = self.connection().await?;
        let transaction = connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        if origin == ImportOrigin::Source
            && !Self::covered(&transaction, host_key.as_deref()).await?
        {
            return Err(GameCatalogError::Uncovered(input.path.to_string()));
        }
        input.parent_binary_id = self.disc_parent(&transaction, &input, binary_id).await?;
        self.validate_parent(&transaction, &input).await?;
        let mut binary_rows = transaction
            .query(
                "SELECT 1 FROM binaries WHERE id = ?1",
                [Self::id(binary_id)],
            )
            .await?;
        let binary_existed = binary_rows.next().await?.is_some();
        drop(binary_rows);
        self.upsert_binary(&transaction, binary_id, &input).await?;

        let mut existing = transaction
            .query(
                r#"SELECT id, binary_id, path FROM launchables
                   WHERE path = ?1 OR (host_key = ?2 AND substr(path, 1, instr(path, ':')) = substr(?1, 1, instr(?1, ':')))
                   ORDER BY path = ?1 DESC LIMIT 1"#,
                vec![
                    Value::from(path.clone()),
                    host_key.clone().map_or(Value::Null, Value::from),
                ],
            )
            .await?;
        let (id, previous_binary) = match existing.next().await? {
            Some(row) => {
                path = row.get(2)?;
                (
                    Self::uuid(row.get_value(0)?)?,
                    Some(Self::uuid(row.get_value(1)?)?),
                )
            }
            None => (Uuid::now_v7(), None),
        };
        drop(existing);
        let reassigned_objects = match previous_binary.filter(|previous| *previous != binary_id) {
            Some(previous) => self.reassignment_objects(&transaction, previous).await?,
            None => Vec::new(),
        };
        transaction
            .execute(
                r#"
                INSERT INTO launchables (id, binary_id, path, entrypoint, package, host_key, install_path, manual)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                ON CONFLICT(path) DO UPDATE SET binary_id = excluded.binary_id, entrypoint = excluded.entrypoint,
                    package = excluded.package, host_key = excluded.host_key, install_path = excluded.install_path,
                    manual = launchables.manual OR excluded.manual
                "#,
                vec![
                    Self::id(id),
                    Self::id(binary_id),
                    Value::from(path),
                    input.entrypoint.clone().map_or(Value::Null, Value::from),
                    input.package.clone().map_or(Value::Null, Value::from),
                    host_key.map_or(Value::Null, Value::from),
                    install_path.map_or(Value::Null, Value::from),
                    Value::from(i64::from(origin == ImportOrigin::Manual)),
                ],
            )
            .await?;
        if let Some(previous) = previous_binary.filter(|previous| *previous != binary_id) {
            transaction
                .execute(
                    r#"DELETE FROM binaries
                       WHERE id = ?1
                         AND NOT EXISTS (SELECT 1 FROM launchables WHERE binary_id = ?1)
                         AND NOT EXISTS (SELECT 1 FROM binaries WHERE parent_binary_id = ?1)"#,
                    [Self::id(previous)],
                )
                .await?;
        }
        Self::write_manifest(&transaction, id, input.manifest.as_ref()).await?;
        let mut replaced_objects = Vec::new();
        for (producer, values) in prepared_seeds {
            replaced_objects
                .extend(Self::referenced_objects(&transaction, binary_id, Some(&producer)).await?);
            transaction
                .execute(
                    "DELETE FROM metadata WHERE parent_binary_id = ?1 AND producer = ?2",
                    vec![Self::id(binary_id), Value::from(producer)],
                )
                .await?;
            for value in values {
                self.insert_metadata(&transaction, Uuid::now_v7(), binary_id, &value)
                    .await?;
            }
        }
        transaction.commit().await?;
        replaced_objects.extend(reassigned_objects);
        self.collect_objects(replaced_objects).await;
        Ok((
            Launchable {
                id,
                binary_id,
                identified: input,
            },
            !binary_existed,
        ))
    }

    pub async fn remove_launchable(&self, id: Uuid) -> Result<(), GameCatalogError> {
        let mut connection = self.connection().await?;
        let transaction = connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        let mut rows = transaction
            .query(
                r#"SELECT binary.id, binary.parent_binary_id,
                          (SELECT COUNT(*) FROM launchables WHERE binary_id = binary.id),
                          (SELECT COUNT(*) FROM binaries WHERE parent_binary_id = binary.id)
                   FROM launchables JOIN binaries AS binary ON binary.id = launchables.binary_id
                   WHERE launchables.id = ?1"#,
                [Self::id(id)],
            )
            .await?;
        let row = rows
            .next()
            .await?
            .ok_or_else(|| GameCatalogError::NotFound(id.simple().to_string()))?;
        let binary_id = Self::uuid(row.get_value(0)?)?;
        let parent = Self::optional_uuid(row.get_value(1)?)?;
        let launchable_count: i64 = row.get(2)?;
        let child_count: i64 = row.get(3)?;
        drop(rows);
        if parent.is_none() && launchable_count == 1 {
            if child_count != 0 {
                return Err(GameCatalogError::InvalidData(
                    "the root launchable cannot be removed while the game has additional discs"
                        .to_owned(),
                ));
            }
            let objects = Self::referenced_objects(&transaction, binary_id, None).await?;
            transaction
                .execute("DELETE FROM binaries WHERE id = ?1", [Self::id(binary_id)])
                .await?;
            transaction.commit().await?;
            self.collect_objects(objects).await;
            return Ok(());
        }
        transaction
            .execute("DELETE FROM launchables WHERE id = ?1", [Self::id(id)])
            .await?;
        transaction
            .execute(
                r#"DELETE FROM binaries
                   WHERE id = ?1 AND NOT EXISTS (SELECT 1 FROM launchables WHERE binary_id = ?1)
                     AND NOT EXISTS (SELECT 1 FROM binaries WHERE parent_binary_id = ?1)
                     AND NOT EXISTS (SELECT 1 FROM metadata WHERE parent_binary_id = ?1)"#,
                [Self::id(binary_id)],
            )
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn delete_game(&self, id: Uuid) -> Result<(), GameCatalogError> {
        let mut connection = self.connection().await?;
        let transaction = connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        self.require_root(&transaction, id).await?;
        let objects = Self::referenced_objects(&transaction, id, None).await?;
        transaction
            .execute(
                "DELETE FROM binaries WHERE parent_binary_id = ?1",
                [Self::id(id)],
            )
            .await?;
        transaction
            .execute("DELETE FROM binaries WHERE id = ?1", [Self::id(id)])
            .await?;
        transaction.commit().await?;
        self.collect_objects(objects).await;
        Ok(())
    }

    async fn initialize(&self) -> Result<(), GameCatalogError> {
        let connection = self.connection().await?;
        let mut rows = connection.query("PRAGMA user_version", ()).await?;
        let version = rows
            .next()
            .await?
            .ok_or_else(|| GameCatalogError::InvalidData("missing schema version".to_owned()))?
            .get::<i64>(0)?;
        drop(rows);
        match version {
            0 => connection.execute_batch(SCHEMA).await?,
            13 => {}
            version => {
                return Err(GameCatalogError::InvalidData(format!(
                    "library database schema version {version} is not supported"
                )));
            }
        }
        Ok(())
    }

    pub(super) async fn connection(&self) -> Result<turso::Connection, GameCatalogError> {
        let connection = self.database.connect()?;
        connection.execute("PRAGMA foreign_keys = ON", ()).await?;
        Ok(connection)
    }

    async fn validate_parent(
        &self,
        transaction: &Transaction<'_>,
        input: &IdentifiedLaunchable,
    ) -> Result<(), GameCatalogError> {
        let Some(parent) = input.parent_binary_id else {
            return Ok(());
        };
        let mut rows = transaction
            .query(
                "SELECT platform, parent_binary_id FROM binaries WHERE id = ?1",
                [Self::id(parent)],
            )
            .await?;
        let row = rows
            .next()
            .await?
            .ok_or_else(|| GameCatalogError::NotFound(parent.simple().to_string()))?;
        if row.get::<String>(0)? != input.platform || !row.get_value(1)?.is_null() {
            return Err(GameCatalogError::InvalidData(
                "a disc parent must be a root binary on the same platform".to_owned(),
            ));
        }
        Ok(())
    }

    async fn upsert_binary(
        &self,
        transaction: &Transaction<'_>,
        id: Uuid,
        input: &IdentifiedLaunchable,
    ) -> Result<(), GameCatalogError> {
        let mut rows = transaction
            .query(
                "SELECT platform, serial, title_id, parent_binary_id FROM binaries WHERE id = ?1",
                [Self::id(id)],
            )
            .await?;
        if let Some(row) = rows.next().await? {
            let platform: String = row.get(0)?;
            let serial = Self::optional_text(row.get_value(1)?)?;
            let title_id = Self::optional_text(row.get_value(2)?)?;
            let parent = Self::optional_uuid(row.get_value(3)?)?;
            if platform != input.platform
                || parent != input.parent_binary_id
                || Self::conflicts(serial.as_deref(), input.serial.as_deref())
                || Self::conflicts(title_id.as_deref(), input.title_id.as_deref())
            {
                return Err(GameCatalogError::InvalidData(
                    "the same binary hash has conflicting identity data".to_owned(),
                ));
            }
            drop(rows);
            transaction
                .execute(
                    "UPDATE binaries SET serial = COALESCE(serial, ?2), title_id = COALESCE(title_id, ?3), requires_license = ?4 WHERE id = ?1",
                    vec![
                        Self::id(id),
                        input.serial.clone().into(),
                        input.title_id.clone().into(),
                        Value::from(i64::from(input.requires_license)),
                    ],
                )
                .await?;
            return Ok(());
        }
        drop(rows);
        transaction
            .execute(
                r#"INSERT INTO binaries (id, blake3, platform, serial, title_id, disc_number, requires_license, parent_binary_id)
                   VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"#,
                vec![
                    Self::id(id),
                    Value::from(input.binary_blake3.to_vec()),
                    Value::from(input.platform.clone()),
                    input.serial.clone().into(),
                    input.title_id.clone().into(),
                    input.disc_number.map_or(Value::Null, Value::from),
                    Value::from(i64::from(input.requires_license)),
                    input.parent_binary_id.map(Self::id).unwrap_or(Value::Null),
                ],
            )
            .await?;
        Ok(())
    }

    async fn reassignment_objects(
        &self,
        transaction: &Transaction<'_>,
        binary: Uuid,
    ) -> Result<Vec<[u8; 32]>, GameCatalogError> {
        let mut rows = transaction
            .query(
                r#"SELECT parent_binary_id,
                          (SELECT COUNT(*) FROM launchables WHERE binary_id = ?1),
                          (SELECT COUNT(*) FROM binaries WHERE parent_binary_id = ?1)
                   FROM binaries WHERE id = ?1"#,
                [Self::id(binary)],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(Vec::new());
        };
        let is_root = row.get_value(0)?.is_null();
        let launchables: i64 = row.get(1)?;
        let children: i64 = row.get(2)?;
        if is_root && launchables <= 1 && children > 0 {
            return Err(GameCatalogError::InvalidData(
                "the root launchable cannot be reassigned while the game has additional discs"
                    .to_owned(),
            ));
        }
        if is_root && launchables <= 1 {
            Self::referenced_objects(transaction, binary, None).await
        } else {
            Ok(Vec::new())
        }
    }

    /// The root binary of `binary`, which is `binary` itself unless it is a disc of a game.
    pub async fn root_binary(&self, binary: Uuid) -> Result<Uuid, GameCatalogError> {
        let connection = self.connection().await?;
        let mut rows = connection
            .query(
                "SELECT COALESCE(parent_binary_id, id) FROM binaries WHERE id = ?1",
                [Self::id(binary)],
            )
            .await?;
        rows.next()
            .await?
            .ok_or_else(|| GameCatalogError::NotFound(binary.simple().to_string()))
            .and_then(|row| Self::uuid(row.get_value(0)?))
    }

    pub async fn requires_license(
        &self,
        platform: &str,
        title_id: &str,
    ) -> Result<bool, GameCatalogError> {
        let connection = self.connection().await?;
        let mut rows = connection
            .query(
                "SELECT 1 FROM binaries WHERE platform = ?1 AND title_id = ?2 AND requires_license = 1 LIMIT 1",
                vec![Value::from(platform), Value::from(title_id)],
            )
            .await?;
        Ok(rows.next().await?.is_some())
    }

    pub(super) async fn require_root(
        &self,
        transaction: &Transaction<'_>,
        id: Uuid,
    ) -> Result<(), GameCatalogError> {
        let mut rows = transaction
            .query(
                "SELECT parent_binary_id FROM binaries WHERE id = ?1",
                [Self::id(id)],
            )
            .await?;
        let row = rows
            .next()
            .await?
            .ok_or_else(|| GameCatalogError::NotFound(id.simple().to_string()))?;
        if !row.get_value(0)?.is_null() {
            return Err(GameCatalogError::InvalidData(
                "metadata and game operations require a root binary".to_owned(),
            ));
        }
        Ok(())
    }

    pub(super) async fn require_binary(
        &self,
        transaction: &Transaction<'_>,
        id: Uuid,
    ) -> Result<(), GameCatalogError> {
        let mut rows = transaction
            .query("SELECT 1 FROM binaries WHERE id = ?1", [Self::id(id)])
            .await?;
        if rows.next().await?.is_none() {
            return Err(GameCatalogError::NotFound(id.simple().to_string()));
        }
        Ok(())
    }

    fn metadata_from_row(row: &turso::Row) -> Result<Metadata, GameCatalogError> {
        let kind: String = row.get(6)?;
        let data = match kind.as_str() {
            "string" => MetadataData::Scalar(MetadataValue::String(row.get(7)?)),
            "number" => MetadataData::Scalar(MetadataValue::Number(row.get(8)?)),
            "boolean" => MetadataData::Scalar(MetadataValue::Boolean(row.get::<i64>(9)? != 0)),
            "inline" => MetadataData::Media {
                media_type: row.get(12)?,
                data: MediaData::Inline(Self::blob(row.get_value(10)?)?.into_boxed_slice()),
                source_url: Self::optional_text(row.get_value(13)?)?,
                preview: Self::preview_from_row(row)?,
            },
            "object" => MetadataData::Media {
                media_type: row.get(12)?,
                data: MediaData::Object(ObjectPath::from(row.get::<String>(11)?)),
                source_url: Self::optional_text(row.get_value(13)?)?,
                preview: Self::preview_from_row(row)?,
            },
            _ => {
                return Err(GameCatalogError::InvalidData(format!(
                    "unknown metadata data kind '{kind}'"
                )));
            }
        };
        Ok(Metadata {
            id: Self::uuid(row.get_value(0)?)?,
            parent_binary_id: Self::uuid(row.get_value(1)?)?,
            key: row.get(2)?,
            source: row.get(3)?,
            producer: row.get(4)?,
            ordinal: u32::try_from(row.get::<i64>(5)?).map_err(|_| {
                GameCatalogError::InvalidData("invalid metadata ordinal".to_owned())
            })?,
            data,
        })
    }

    fn preview_from_row(row: &turso::Row) -> Result<Option<MediaPreview>, GameCatalogError> {
        let dimension = |index| -> Result<Option<u32>, GameCatalogError> {
            match row.get_value(index)? {
                Value::Null => Ok(None),
                value => u32::try_from(value.as_integer().copied().unwrap_or_default())
                    .map(Some)
                    .map_err(|_| {
                        GameCatalogError::InvalidData("invalid media dimension".to_owned())
                    }),
            }
        };
        Ok(
            match (
                dimension(14)?,
                dimension(15)?,
                Self::optional_text(row.get_value(16)?)?,
            ) {
                (Some(width), Some(height), Some(blurhash)) => Some(MediaPreview {
                    width,
                    height,
                    blurhash,
                }),
                _ => None,
            },
        )
    }

    fn validate_media_type(value: &str) -> Result<(), GameCatalogError> {
        value
            .parse::<mime::Mime>()
            .map(|_| ())
            .map_err(|_| GameCatalogError::InvalidData(format!("invalid media type '{value}'")))
    }

    fn conflicts(stored: Option<&str>, incoming: Option<&str>) -> bool {
        matches!((stored, incoming), (Some(stored), Some(incoming)) if stored != incoming)
    }

    pub(super) fn id(value: Uuid) -> Value {
        Value::from(value.as_bytes().to_vec())
    }

    pub(super) fn uuid(value: Value) -> Result<Uuid, GameCatalogError> {
        Self::uuid_value(value)
    }

    fn uuid_value(value: Value) -> Result<Uuid, GameCatalogError> {
        let bytes = Self::blob_value(value)?;
        Uuid::from_slice(&bytes)
            .map_err(|error| GameCatalogError::InvalidData(format!("invalid UUID: {error}")))
    }

    fn optional_uuid(value: Value) -> Result<Option<Uuid>, GameCatalogError> {
        match value {
            Value::Null => Ok(None),
            value => Self::uuid_value(value).map(Some),
        }
    }

    fn blob(value: Value) -> Result<Vec<u8>, GameCatalogError> {
        Self::blob_value(value)
    }

    fn blob_value(value: Value) -> Result<Vec<u8>, GameCatalogError> {
        match value {
            Value::Blob(bytes) => Ok(bytes),
            _ => Err(GameCatalogError::InvalidData(
                "expected database blob".to_owned(),
            )),
        }
    }

    pub(super) fn optional_text(value: Value) -> Result<Option<String>, GameCatalogError> {
        match value {
            Value::Null => Ok(None),
            Value::Text(value) => Ok(Some(value)),
            _ => Err(GameCatalogError::InvalidData(
                "expected database text".to_owned(),
            )),
        }
    }

    fn optional_digest(value: Value) -> Result<Option<[u8; 32]>, GameCatalogError> {
        match value {
            Value::Null => Ok(None),
            Value::Blob(bytes) => bytes.try_into().map(Some).map_err(|_| {
                GameCatalogError::InvalidData("invalid BLAKE3 digest length".to_owned())
            }),
            _ => Err(GameCatalogError::InvalidData(
                "expected database digest".to_owned(),
            )),
        }
    }
}
