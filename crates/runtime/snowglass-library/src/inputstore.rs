use std::{
    collections::BTreeMap,
    path::Path,
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS input_mapping_profiles (
    identity       TEXT NOT NULL,
    identity_scope TEXT NOT NULL CHECK (identity_scope IN ('device', 'type')),
    layout_id      TEXT NOT NULL,
    mode           TEXT NOT NULL CHECK (mode IN ('name', 'position', 'custom')),
    revision       INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
    updated_at_ms  INTEGER NOT NULL,
    PRIMARY KEY (identity, layout_id)
) STRICT;

CREATE TABLE IF NOT EXISTS input_mapping_bindings (
    identity          TEXT NOT NULL,
    layout_id         TEXT NOT NULL,
    target_expression TEXT NOT NULL,
    source_expression TEXT NOT NULL,
    PRIMARY KEY (identity, layout_id, target_expression),
    FOREIGN KEY (identity, layout_id)
        REFERENCES input_mapping_profiles (identity, layout_id)
        ON DELETE CASCADE
) STRICT;

PRAGMA user_version = 1;
"#;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputIdentityScope {
    Device,
    Type,
}

impl InputIdentityScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Device => "device",
            Self::Type => "type",
        }
    }
}

impl FromStr for InputIdentityScope {
    type Err = InputStoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "device" => Ok(Self::Device),
            "type" => Ok(Self::Type),
            _ => Err(InputStoreError::InvalidData(format!(
                "unknown input identity scope '{value}'"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputMappingMode {
    Name,
    Position,
    Custom,
}

impl InputMappingMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Position => "position",
            Self::Custom => "custom",
        }
    }
}

impl FromStr for InputMappingMode {
    type Err = InputStoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "name" => Ok(Self::Name),
            "position" => Ok(Self::Position),
            "custom" => Ok(Self::Custom),
            _ => Err(InputStoreError::InvalidData(format!(
                "unknown input mapping mode '{value}'"
            ))),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputMappingProfile {
    pub identity: String,
    pub identity_scope: InputIdentityScope,
    pub layout_id: String,
    pub mode: InputMappingMode,
    pub revision: u64,
    pub bindings: BTreeMap<String, String>,
}

#[derive(Debug, thiserror::Error)]
pub enum InputStoreError {
    #[error("could not create the input database directory: {0}")]
    CreateDirectory(#[source] std::io::Error),
    #[error("the input database path is not valid UTF-8")]
    InvalidPath,
    #[error("input database error: {0}")]
    Database(#[from] turso::Error),
    #[error("invalid input database data: {0}")]
    InvalidData(String),
    #[error("the system clock is earlier than the Unix epoch")]
    InvalidClock,
}

pub struct InputStore {
    connection: turso::Connection,
}

impl InputStore {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, InputStoreError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(InputStoreError::CreateDirectory)?;
        }
        let path = path.to_str().ok_or(InputStoreError::InvalidPath)?;
        let database = turso::Builder::new_local(path).build().await?;
        let connection = database.connect()?;
        connection.execute("PRAGMA foreign_keys = ON", ()).await?;
        let mut rows = connection.query("PRAGMA user_version", ()).await?;
        let version = rows
            .next()
            .await?
            .ok_or_else(|| {
                InputStoreError::InvalidData("input database has no schema version".to_owned())
            })?
            .get::<i64>(0)?;
        drop(rows);
        match version {
            0 => connection.execute_batch(SCHEMA).await?,
            1 => {}
            version => {
                return Err(InputStoreError::InvalidData(format!(
                    "input database schema version {version} is not supported"
                )));
            }
        }
        Ok(Self { connection })
    }

    pub async fn profiles(&self) -> Result<Vec<InputMappingProfile>, InputStoreError> {
        let mut rows = self
            .connection
            .query(
                r#"
                SELECT
                    profile.identity,
                    profile.identity_scope,
                    profile.layout_id,
                    profile.mode,
                    profile.revision,
                    binding.target_expression,
                    binding.source_expression
                FROM input_mapping_profiles AS profile
                LEFT JOIN input_mapping_bindings AS binding
                    ON binding.identity = profile.identity
                    AND binding.layout_id = profile.layout_id
                ORDER BY profile.identity, profile.layout_id, binding.target_expression
                "#,
                (),
            )
            .await?;
        let mut profiles = BTreeMap::<(String, String), InputMappingProfile>::new();
        while let Some(row) = rows.next().await? {
            let identity: String = row.get(0)?;
            let layout_id: String = row.get(2)?;
            let key = (identity.clone(), layout_id.clone());
            let profile = profiles.entry(key).or_insert(InputMappingProfile {
                identity,
                identity_scope: row.get::<String>(1)?.parse()?,
                layout_id,
                mode: row.get::<String>(3)?.parse()?,
                revision: u64::try_from(row.get::<i64>(4)?)
                    .ok()
                    .filter(|revision| *revision > 0)
                    .ok_or_else(|| {
                        InputStoreError::InvalidData("mapping revision must be positive".to_owned())
                    })?,
                bindings: BTreeMap::new(),
            });
            if let (turso::Value::Text(target), turso::Value::Text(source)) =
                (row.get_value(5)?, row.get_value(6)?)
            {
                profile.bindings.insert(target, source);
            }
        }
        Ok(profiles.into_values().collect())
    }

    pub async fn mapping(
        &self,
        identity: &str,
        layout_id: &str,
    ) -> Result<Option<InputMappingProfile>, InputStoreError> {
        let mut rows = self
            .connection
            .query(
                r#"
                SELECT identity_scope, mode, revision
                FROM input_mapping_profiles
                WHERE identity = ?1 AND layout_id = ?2
                "#,
                (identity, layout_id),
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let mut profile = InputMappingProfile {
            identity: identity.to_owned(),
            identity_scope: row.get::<String>(0)?.parse()?,
            layout_id: layout_id.to_owned(),
            mode: row.get::<String>(1)?.parse()?,
            revision: u64::try_from(row.get::<i64>(2)?)
                .ok()
                .filter(|revision| *revision > 0)
                .ok_or_else(|| {
                    InputStoreError::InvalidData("mapping revision must be positive".to_owned())
                })?,
            bindings: BTreeMap::new(),
        };
        drop(rows);
        let mut bindings = self
            .connection
            .query(
                r#"
                SELECT target_expression, source_expression
                FROM input_mapping_bindings
                WHERE identity = ?1 AND layout_id = ?2
                ORDER BY target_expression
                "#,
                (identity, layout_id),
            )
            .await?;
        while let Some(row) = bindings.next().await? {
            profile.bindings.insert(row.get(0)?, row.get(1)?);
        }
        Ok(Some(profile))
    }

    pub async fn put_mapping(
        &mut self,
        profile: &InputMappingProfile,
    ) -> Result<u64, InputStoreError> {
        if profile.mode != InputMappingMode::Custom && !profile.bindings.is_empty() {
            return Err(InputStoreError::InvalidData(
                "name and position mappings cannot store explicit bindings".to_owned(),
            ));
        }
        let updated_at_ms = i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| InputStoreError::InvalidClock)?
                .as_millis(),
        )
        .map_err(|_| InputStoreError::InvalidClock)?;
        let transaction = self
            .connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        let mut rows = transaction
            .query(
                r#"
                SELECT revision
                FROM input_mapping_profiles
                WHERE identity = ?1 AND layout_id = ?2
                "#,
                (profile.identity.as_str(), profile.layout_id.as_str()),
            )
            .await?;
        let revision = match rows.next().await? {
            Some(row) => row
                .get::<i64>(0)?
                .checked_add(1)
                .ok_or_else(|| InputStoreError::InvalidData("mapping revision overflow".into()))?,
            None => 1,
        };
        drop(rows);
        transaction
            .execute(
                r#"
                INSERT INTO input_mapping_profiles (
                    identity, identity_scope, layout_id, mode, revision, updated_at_ms
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ON CONFLICT (identity, layout_id) DO UPDATE SET
                    identity_scope = excluded.identity_scope,
                    mode = excluded.mode,
                    revision = excluded.revision,
                    updated_at_ms = excluded.updated_at_ms
                "#,
                (
                    profile.identity.as_str(),
                    profile.identity_scope.as_str(),
                    profile.layout_id.as_str(),
                    profile.mode.as_str(),
                    revision,
                    updated_at_ms,
                ),
            )
            .await?;
        transaction
            .execute(
                "DELETE FROM input_mapping_bindings WHERE identity = ?1 AND layout_id = ?2",
                (profile.identity.as_str(), profile.layout_id.as_str()),
            )
            .await?;
        for (target, source) in &profile.bindings {
            transaction
                .execute(
                    r#"
                    INSERT INTO input_mapping_bindings (
                        identity, layout_id, target_expression, source_expression
                    ) VALUES (?1, ?2, ?3, ?4)
                    "#,
                    (
                        profile.identity.as_str(),
                        profile.layout_id.as_str(),
                        target.as_str(),
                        source.as_str(),
                    ),
                )
                .await?;
        }
        transaction.commit().await?;
        u64::try_from(revision).map_err(|_| {
            InputStoreError::InvalidData("mapping revision must be positive".to_owned())
        })
    }

    pub async fn remove_mapping(
        &mut self,
        identity: &str,
        layout_id: &str,
    ) -> Result<(), InputStoreError> {
        self.connection
            .execute(
                "DELETE FROM input_mapping_profiles WHERE identity = ?1 AND layout_id = ?2",
                (identity, layout_id),
            )
            .await?;
        Ok(())
    }
}
