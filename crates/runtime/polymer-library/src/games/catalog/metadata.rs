use object_store::path::Path as ObjectPath;
use turso::{Value, transaction::Transaction};
use uuid::Uuid;

use super::{
    GameCatalog, GameCatalogError, MediaData, MediaPreview, MediaResource, Metadata, MetadataData,
    MetadataInput, MetadataInputData, MetadataValue, PreparedMetadata, PreparedMetadataData,
};

impl GameCatalog {
    pub async fn add_metadata(
        &self,
        parent: Uuid,
        input: MetadataInput,
    ) -> Result<Metadata, GameCatalogError> {
        let connection = self.connection().await?;
        let mut rows = connection.query(
            "SELECT COALESCE(MAX(ordinal), -1) + 1 FROM metadata WHERE parent_binary_id = ?1 AND producer = 'manual'",
            [Self::id(parent)],
        ).await?;
        let ordinal = u32::try_from(rows.next().await?.expect("aggregate row").get::<i64>(0)?)
            .map_err(|_| GameCatalogError::InvalidData("metadata ordinal overflow".to_owned()))?;
        drop(rows);
        self.write_metadata(Uuid::now_v7(), parent, "manual", ordinal, input, false)
            .await
    }

    pub async fn update_metadata(
        &self,
        id: Uuid,
        input: MetadataInput,
    ) -> Result<Metadata, GameCatalogError> {
        let connection = self.connection().await?;
        let mut rows = connection
            .query(
                "SELECT parent_binary_id, producer, ordinal FROM metadata WHERE id = ?1",
                [Self::id(id)],
            )
            .await?;
        let row = rows
            .next()
            .await?
            .ok_or_else(|| GameCatalogError::NotFound(id.simple().to_string()))?;
        let parent = Self::uuid(row.get_value(0)?)?;
        let producer: String = row.get(1)?;
        let ordinal = u32::try_from(row.get::<i64>(2)?)
            .map_err(|_| GameCatalogError::InvalidData("invalid metadata ordinal".to_owned()))?;
        drop(rows);
        self.write_metadata(id, parent, &producer, ordinal, input, true)
            .await
    }

    pub async fn remove_metadata(&self, id: Uuid) -> Result<(), GameCatalogError> {
        let mut connection = self.connection().await?;
        let transaction = connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        let mut rows = transaction
            .query(
                "SELECT object_blake3 FROM metadata WHERE id = ?1",
                [Self::id(id)],
            )
            .await?;
        let old = rows
            .next()
            .await?
            .map(|row| Self::optional_digest(row.get_value(0)?))
            .transpose()?
            .ok_or_else(|| GameCatalogError::NotFound(id.simple().to_string()))?;
        drop(rows);
        transaction
            .execute("DELETE FROM metadata WHERE id = ?1", [Self::id(id)])
            .await?;
        transaction.commit().await?;
        self.collect_objects(old.into_iter().collect()).await;
        Ok(())
    }

    /// Atomically replaces all values owned by one host-controlled producer.
    pub async fn replace_metadata(
        &self,
        parent: Uuid,
        producer: &str,
        inputs: Vec<MetadataInput>,
    ) -> Result<Vec<Metadata>, GameCatalogError> {
        Self::validate_context("producer", producer)?;
        let mut prepared = Vec::with_capacity(inputs.len());
        for (ordinal, input) in inputs.into_iter().enumerate() {
            let ordinal = u32::try_from(ordinal).map_err(|_| {
                GameCatalogError::InvalidData("too many metadata values".to_owned())
            })?;
            prepared.push(self.prepare_metadata(producer, ordinal, input).await?);
        }
        let mut connection = self.connection().await?;
        let transaction = connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        self.require_binary(&transaction, parent).await?;
        let old = Self::referenced_objects(&transaction, parent, Some(producer)).await?;
        transaction
            .execute(
                "DELETE FROM metadata WHERE parent_binary_id = ?1 AND producer = ?2",
                vec![Self::id(parent), Value::from(producer)],
            )
            .await?;
        let mut result = Vec::with_capacity(prepared.len());
        for value in prepared {
            let id = Uuid::now_v7();
            self.insert_metadata(&transaction, id, parent, &value)
                .await?;
            result.push(value.into_metadata(id, parent));
        }
        transaction.commit().await?;
        self.collect_objects(old).await;
        Ok(result)
    }

    pub async fn metadata(&self, parent: Uuid) -> Result<Vec<Metadata>, GameCatalogError> {
        let connection = self.connection().await?;
        let mut rows = connection.query(
            r#"SELECT metadata.id, metadata.parent_binary_id, metadata.key, metadata.source,
                      metadata.producer, metadata.ordinal, metadata.data_kind,
                      metadata.string_value, metadata.number_value, metadata.boolean_value,
                      metadata.inline_data, media_objects.object_key, metadata.media_type,
                      metadata.source_url, metadata.width, metadata.height, metadata.blurhash
               FROM metadata LEFT JOIN media_objects ON media_objects.blake3 = metadata.object_blake3
               WHERE metadata.parent_binary_id = ?1
               ORDER BY metadata.key, metadata.source, metadata.ordinal, metadata.id"#,
            [Self::id(parent)],
        ).await?;
        let mut values = Vec::new();
        while let Some(row) = rows.next().await? {
            values.push(Self::metadata_from_row(&row)?);
        }
        Ok(values)
    }

    pub async fn metadata_text(&self, id: Uuid) -> Result<String, GameCatalogError> {
        let connection = self.connection().await?;
        let mut rows = connection.query(
            "SELECT data_kind, string_value, number_value, boolean_value FROM metadata WHERE id = ?1",
            [Self::id(id)],
        ).await?;
        let row = rows
            .next()
            .await?
            .ok_or_else(|| GameCatalogError::NotFound(id.simple().to_string()))?;
        Ok(match row.get::<String>(0)?.as_str() {
            "string" => row.get(1)?,
            "number" => row.get::<f64>(2)?.to_string(),
            "boolean" => (row.get::<i64>(3)? != 0).to_string(),
            _ => {
                return Err(GameCatalogError::InvalidData(
                    "media metadata does not have text".to_owned(),
                ));
            }
        })
    }

    pub async fn media(&self, id: Uuid) -> Result<MediaResource, GameCatalogError> {
        let connection = self.connection().await?;
        let mut rows = connection.query(
            r#"SELECT metadata.data_kind, metadata.inline_data, media_objects.object_key,
                      metadata.media_type, metadata.byte_length, metadata.object_blake3
               FROM metadata LEFT JOIN media_objects ON media_objects.blake3 = metadata.object_blake3
               WHERE metadata.id = ?1"#,
            [Self::id(id)],
        ).await?;
        let row = rows
            .next()
            .await?
            .ok_or_else(|| GameCatalogError::NotFound(id.simple().to_string()))?;
        let media_type = Self::optional_text(row.get_value(3)?)?.ok_or_else(|| {
            GameCatalogError::InvalidData("scalar metadata does not have a URL".to_owned())
        })?;
        let byte_length = u64::try_from(row.get::<i64>(4)?)
            .map_err(|_| GameCatalogError::InvalidData("negative media length".to_owned()))?;
        let (data, content_blake3) = match row.get::<String>(0)?.as_str() {
            "inline" => {
                let bytes = Self::blob(row.get_value(1)?)?;
                let digest = *blake3::hash(&bytes).as_bytes();
                (MediaData::Inline(bytes.into_boxed_slice()), digest)
            }
            "object" => {
                let key = Self::optional_text(row.get_value(2)?)?.ok_or_else(|| {
                    GameCatalogError::InvalidData("object metadata has no object key".to_owned())
                })?;
                let digest = Self::optional_digest(row.get_value(5)?)?.ok_or_else(|| {
                    GameCatalogError::InvalidData("object metadata has no digest".to_owned())
                })?;
                (MediaData::Object(ObjectPath::from(key)), digest)
            }
            _ => {
                return Err(GameCatalogError::InvalidData(
                    "scalar metadata does not have a URL".to_owned(),
                ));
            }
        };
        Ok(MediaResource {
            metadata_id: id,
            media_type,
            byte_length,
            data,
            content_blake3,
        })
    }

    async fn write_metadata(
        &self,
        id: Uuid,
        parent: Uuid,
        producer: &str,
        ordinal: u32,
        input: MetadataInput,
        update: bool,
    ) -> Result<Metadata, GameCatalogError> {
        let value = self.prepare_metadata(producer, ordinal, input).await?;
        let mut connection = self.connection().await?;
        let transaction = connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        self.require_binary(&transaction, parent).await?;
        let old = if update {
            let mut rows = transaction
                .query(
                    "SELECT object_blake3 FROM metadata WHERE id = ?1",
                    [Self::id(id)],
                )
                .await?;
            let old = rows
                .next()
                .await?
                .map(|row| Self::optional_digest(row.get_value(0)?))
                .transpose()?
                .ok_or_else(|| GameCatalogError::NotFound(id.simple().to_string()))?;
            drop(rows);
            transaction
                .execute("DELETE FROM metadata WHERE id = ?1", [Self::id(id)])
                .await?;
            old
        } else {
            None
        };
        self.insert_metadata(&transaction, id, parent, &value)
            .await?;
        transaction.commit().await?;
        self.collect_objects(old.into_iter().collect()).await;
        Ok(value.into_metadata(id, parent))
    }

    pub(super) async fn prepare_metadata(
        &self,
        producer: &str,
        ordinal: u32,
        input: MetadataInput,
    ) -> Result<PreparedMetadata, GameCatalogError> {
        Self::validate_key(&input.key)?;
        Self::validate_context("source", &input.source)?;
        Self::validate_context("producer", producer)?;
        let data = match input.data {
            MetadataInputData::Scalar { value } => {
                value
                    .validate()
                    .map_err(|message| GameCatalogError::InvalidData(message.to_owned()))?;
                PreparedMetadataData::Scalar(value)
            }
            MetadataInputData::Inline {
                media_type,
                bytes,
                source_url,
            } => {
                Self::validate_media_type(&media_type)?;
                i64::try_from(bytes.len())
                    .map_err(|_| GameCatalogError::InvalidData("media is too large".to_owned()))?;
                let preview = Self::preview(&media_type, bytes.to_vec()).await;
                PreparedMetadataData::Inline {
                    media_type,
                    bytes,
                    source_url,
                    preview,
                }
            }
            MetadataInputData::Object {
                media_type,
                path,
                source_url,
            } => {
                Self::validate_media_type(&media_type)?;
                let object = self.store_object(&path).await?;
                let preview = match tokio::fs::read(&path).await {
                    Ok(bytes) => Self::preview(&media_type, bytes).await,
                    Err(_) => None,
                };
                PreparedMetadataData::Object {
                    media_type,
                    object,
                    source_url,
                    preview,
                }
            }
        };
        Ok(PreparedMetadata {
            key: input.key,
            source: input.source,
            producer: producer.to_owned(),
            ordinal,
            data,
        })
    }

    async fn preview(media_type: &str, bytes: Vec<u8>) -> Option<MediaPreview> {
        if !media_type.starts_with("image/") {
            return None;
        }
        tokio::task::spawn_blocking(move || MediaPreview::from_image(&bytes))
            .await
            .ok()
            .flatten()
    }

    fn validate_key(key: &str) -> Result<(), GameCatalogError> {
        if key.is_empty()
            || key.split('.').any(|segment| {
                segment.is_empty()
                    || !segment
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            })
        {
            return Err(GameCatalogError::InvalidData(format!(
                "invalid namespaced metadata key '{key}'"
            )));
        }
        Ok(())
    }

    fn validate_context(name: &str, value: &str) -> Result<(), GameCatalogError> {
        if value.trim().is_empty() {
            return Err(GameCatalogError::InvalidData(format!(
                "metadata {name} cannot be empty"
            )));
        }
        Ok(())
    }

    pub(super) async fn insert_metadata(
        &self,
        transaction: &Transaction<'_>,
        id: Uuid,
        parent: Uuid,
        value: &PreparedMetadata,
    ) -> Result<(), GameCatalogError> {
        if let PreparedMetadataData::Object { object, .. } = &value.data {
            transaction.execute(
                "INSERT INTO media_objects (blake3, object_key, byte_length) VALUES (?1, ?2, ?3) ON CONFLICT(blake3) DO NOTHING",
                vec![Value::from(object.digest.to_vec()), Value::from(object.key.to_string()), Value::from(i64::try_from(object.byte_length).map_err(|_| GameCatalogError::InvalidData("media is too large".to_owned()))?)],
            ).await?;
        }
        let mut parameters = vec![
            Self::id(id),
            Self::id(parent),
            Value::from(value.key.clone()),
            Value::from(value.source.clone()),
            Value::from(value.producer.clone()),
            Value::from(i64::from(value.ordinal)),
        ];
        parameters.extend(value.data.database_values()?);
        transaction.execute(
            r#"INSERT INTO metadata (id, parent_binary_id, key, source, producer, ordinal, data_kind,
                    string_value, number_value, boolean_value, inline_data, object_blake3,
                    media_type, byte_length, source_url, width, height, blurhash)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)"#,
            parameters,
        ).await?;
        Ok(())
    }
}

impl PreparedMetadata {
    fn into_metadata(self, id: Uuid, parent_binary_id: Uuid) -> Metadata {
        let data = match self.data {
            PreparedMetadataData::Scalar(value) => MetadataData::Scalar(value),
            PreparedMetadataData::Inline {
                media_type,
                bytes,
                source_url,
                preview,
            } => MetadataData::Media {
                media_type,
                data: MediaData::Inline(bytes),
                source_url,
                preview,
            },
            PreparedMetadataData::Object {
                media_type,
                object,
                source_url,
                preview,
            } => MetadataData::Media {
                media_type,
                data: MediaData::Object(object.key),
                source_url,
                preview,
            },
        };
        Metadata {
            id,
            parent_binary_id,
            key: self.key,
            source: self.source,
            producer: self.producer,
            ordinal: self.ordinal,
            data,
        }
    }
}

impl PreparedMetadataData {
    fn database_values(&self) -> Result<[Value; 12], GameCatalogError> {
        let preview = match self {
            Self::Inline { preview, .. } | Self::Object { preview, .. } => preview.as_ref(),
            Self::Scalar(_) => None,
        };
        let [width, height, blurhash] =
            preview.map_or([Value::Null, Value::Null, Value::Null], |preview| {
                [
                    Value::from(i64::from(preview.width)),
                    Value::from(i64::from(preview.height)),
                    Value::from(preview.blurhash.clone()),
                ]
            });
        let values =
            match self {
                Self::Scalar(MetadataValue::String(value)) => [
                    Value::from("string"),
                    Value::from(value.clone()),
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                ],
                Self::Scalar(MetadataValue::Number(value)) => [
                    Value::from("number"),
                    Value::Null,
                    Value::from(*value),
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                ],
                Self::Scalar(MetadataValue::Boolean(value)) => [
                    Value::from("boolean"),
                    Value::Null,
                    Value::Null,
                    Value::from(i64::from(*value)),
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                ],
                Self::Inline {
                    media_type,
                    bytes,
                    source_url,
                    ..
                } => [
                    Value::from("inline"),
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::from(bytes.to_vec()),
                    Value::Null,
                    Value::from(media_type.clone()),
                    Value::from(i64::try_from(bytes.len()).map_err(|_| {
                        GameCatalogError::InvalidData("media is too large".to_owned())
                    })?),
                    source_url.clone().into(),
                ],
                Self::Object {
                    media_type,
                    object,
                    source_url,
                    ..
                } => [
                    Value::from("object"),
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::from(object.digest.to_vec()),
                    Value::from(media_type.clone()),
                    Value::from(i64::try_from(object.byte_length).map_err(|_| {
                        GameCatalogError::InvalidData("media is too large".to_owned())
                    })?),
                    source_url.clone().into(),
                ],
            };
        let [
            kind,
            string,
            number,
            boolean,
            inline,
            object,
            media_type,
            byte_length,
            source_url,
        ] = values;
        Ok([
            kind,
            string,
            number,
            boolean,
            inline,
            object,
            media_type,
            byte_length,
            source_url,
            width,
            height,
            blurhash,
        ])
    }
}
