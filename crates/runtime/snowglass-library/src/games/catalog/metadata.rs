use language_tags::LanguageTag;
use object_store::path::Path as ObjectPath;
use turso::{Value, transaction::Transaction};
use uuid::Uuid;

use super::{
    GameCatalog, GameCatalogError, MediaData, MediaResource, Metadata, MetadataData, MetadataInput,
    PreparedMetadata, PreparedMetadataData, Relation,
};

impl GameCatalog {
    pub async fn add_metadata(
        &self,
        parent: Uuid,
        input: MetadataInput,
    ) -> Result<Metadata, GameCatalogError> {
        let id = Uuid::now_v7();
        self.write_metadata(id, parent, input, false).await
    }

    pub async fn update_metadata(
        &self,
        id: Uuid,
        input: MetadataInput,
    ) -> Result<Metadata, GameCatalogError> {
        let connection = self.connection().await?;
        let mut rows = connection
            .query(
                "SELECT parent_binary_id FROM metadata WHERE id = ?1",
                [Self::id(id)],
            )
            .await?;
        let parent = rows
            .next()
            .await?
            .map(|row| Self::uuid(row.get_value(0)?))
            .transpose()?
            .ok_or_else(|| GameCatalogError::NotFound(id.simple().to_string()))?;
        drop(rows);
        self.write_metadata(id, parent, input, true).await
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

    pub async fn replace_metadata(
        &self,
        parent: Uuid,
        source: &str,
        inputs: Vec<MetadataInput>,
    ) -> Result<Vec<Metadata>, GameCatalogError> {
        if source.is_empty() {
            return Err(GameCatalogError::InvalidData(
                "metadata source cannot be empty".to_owned(),
            ));
        }
        let mut prepared = Vec::with_capacity(inputs.len());
        for input in inputs {
            prepared.push(self.prepare_metadata(input.with_source(source)).await?);
        }
        let mut connection = self.connection().await?;
        let transaction = connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        self.require_root(&transaction, parent).await?;
        let old = Self::referenced_objects(&transaction, parent, Some(source)).await?;
        transaction
            .execute(
                "DELETE FROM metadata WHERE parent_binary_id = ?1 AND source = ?2",
                vec![Self::id(parent), Value::from(source)],
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

    pub async fn metadata_text(&self, id: Uuid) -> Result<String, GameCatalogError> {
        let connection = self.connection().await?;
        let mut rows = connection
            .query(
                "SELECT data_kind, text_value, release_year FROM metadata WHERE id = ?1",
                [Self::id(id)],
            )
            .await?;
        let row = rows
            .next()
            .await?
            .ok_or_else(|| GameCatalogError::NotFound(id.simple().to_string()))?;
        match row.get::<String>(0)?.as_str() {
            "text" => Ok(row.get(1)?),
            "year" => Ok(row.get::<i64>(2)?.to_string()),
            _ => Err(GameCatalogError::InvalidData(
                "media metadata does not have text".to_owned(),
            )),
        }
    }

    pub async fn media(&self, id: Uuid) -> Result<MediaResource, GameCatalogError> {
        let connection = self.connection().await?;
        let mut rows = connection
            .query(
                r#"SELECT metadata.data_kind, metadata.inline_data, media_objects.object_key,
                          metadata.media_type, metadata.byte_length, metadata.object_blake3
                   FROM metadata
                   LEFT JOIN media_objects ON media_objects.blake3 = metadata.object_blake3
                   WHERE metadata.id = ?1"#,
                [Self::id(id)],
            )
            .await?;
        let row = rows
            .next()
            .await?
            .ok_or_else(|| GameCatalogError::NotFound(id.simple().to_string()))?;
        let kind: String = row.get(0)?;
        let media_type = Self::optional_text(row.get_value(3)?)?.ok_or_else(|| {
            GameCatalogError::InvalidData("text metadata does not have a URL".to_owned())
        })?;
        let byte_length = u64::try_from(row.get::<i64>(4)?)
            .map_err(|_| GameCatalogError::InvalidData("negative media length".to_owned()))?;
        let (data, content_blake3) = match kind.as_str() {
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
                    "text metadata does not have a URL".to_owned(),
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
        input: MetadataInput,
        update: bool,
    ) -> Result<Metadata, GameCatalogError> {
        let value = self.prepare_metadata(input).await?;
        let mut connection = self.connection().await?;
        let transaction = connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        self.require_root(&transaction, parent).await?;
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

    async fn prepare_metadata(
        &self,
        input: MetadataInput,
    ) -> Result<PreparedMetadata, GameCatalogError> {
        let (language, source) = match &input {
            MetadataInput::Text {
                language, source, ..
            }
            | MetadataInput::ReleaseYear {
                language, source, ..
            }
            | MetadataInput::Inline {
                language, source, ..
            }
            | MetadataInput::Object {
                language, source, ..
            } => (language.as_deref(), source.as_str()),
        };
        let (language, source) = Self::metadata_context(language, source)?;
        let (relation, data) = match input {
            MetadataInput::Text {
                relation, value, ..
            } => {
                if !relation.expects_text() {
                    return Err(GameCatalogError::InvalidData(format!(
                        "{} does not accept text",
                        relation.as_str()
                    )));
                }
                (relation, PreparedMetadataData::Text(value))
            }
            MetadataInput::ReleaseYear { year, .. } => {
                if !(1..=9999).contains(&year) {
                    return Err(GameCatalogError::InvalidData(
                        "release year is out of range".to_owned(),
                    ));
                }
                (
                    Relation::ReleaseDate,
                    PreparedMetadataData::ReleaseYear(year),
                )
            }
            MetadataInput::Inline {
                relation,
                media_type,
                bytes,
                ..
            } => {
                if relation.expects_text() || relation.expects_year() {
                    return Err(GameCatalogError::InvalidData(format!(
                        "{} does not accept media",
                        relation.as_str()
                    )));
                }
                Self::validate_media_type(&media_type)?;
                i64::try_from(bytes.len())
                    .map_err(|_| GameCatalogError::InvalidData("media is too large".to_owned()))?;
                (relation, PreparedMetadataData::Inline { media_type, bytes })
            }
            MetadataInput::Object {
                relation,
                media_type,
                path,
                ..
            } => {
                if relation.expects_text() || relation.expects_year() {
                    return Err(GameCatalogError::InvalidData(format!(
                        "{} does not accept media",
                        relation.as_str()
                    )));
                }
                Self::validate_media_type(&media_type)?;
                let object = self.store_object(&path).await?;
                (
                    relation,
                    PreparedMetadataData::Object { media_type, object },
                )
            }
        };
        Ok(PreparedMetadata {
            relation,
            language,
            source,
            data,
        })
    }

    fn metadata_context(
        language: Option<&str>,
        source: &str,
    ) -> Result<(Option<String>, String), GameCatalogError> {
        if source.is_empty() {
            return Err(GameCatalogError::InvalidData(
                "metadata source cannot be empty".to_owned(),
            ));
        }
        let language = language
            .map(|language| {
                LanguageTag::parse(language)
                    .map(|tag| tag.to_string())
                    .map_err(|_| {
                        GameCatalogError::InvalidData(format!(
                            "invalid BCP 47 language tag '{language}'"
                        ))
                    })
            })
            .transpose()?;
        Ok((language, source.to_owned()))
    }

    async fn insert_metadata(
        &self,
        transaction: &Transaction<'_>,
        id: Uuid,
        parent: Uuid,
        value: &PreparedMetadata,
    ) -> Result<(), GameCatalogError> {
        if let PreparedMetadataData::Object { object, .. } = &value.data {
            transaction
                .execute(
                    r#"INSERT INTO media_objects (blake3, object_key, byte_length)
                       VALUES (?1, ?2, ?3) ON CONFLICT(blake3) DO NOTHING"#,
                    vec![
                        Value::from(object.digest.to_vec()),
                        Value::from(object.key.to_string()),
                        Value::from(i64::try_from(object.byte_length).map_err(|_| {
                            GameCatalogError::InvalidData("media is too large".to_owned())
                        })?),
                    ],
                )
                .await?;
        }
        let columns = value.data.database_values()?;
        let mut parameters = vec![
            Self::id(id),
            Self::id(parent),
            Value::from(value.relation.as_str()),
            value.language.clone().into(),
            Value::from(value.source.clone()),
        ];
        parameters.extend(columns);
        transaction
            .execute(
                r#"INSERT INTO metadata (
                       id, parent_binary_id, relation, language, source, data_kind,
                       text_value, release_year, inline_data, object_blake3, media_type, byte_length
                   ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"#,
                parameters,
            )
            .await?;
        Ok(())
    }
}

impl PreparedMetadata {
    fn into_metadata(self, id: Uuid, parent_binary_id: Uuid) -> Metadata {
        let data = match self.data {
            PreparedMetadataData::Text(value) => MetadataData::Text(value),
            PreparedMetadataData::ReleaseYear(year) => MetadataData::ReleaseYear(year),
            PreparedMetadataData::Inline { media_type, bytes } => MetadataData::Media {
                media_type,
                data: MediaData::Inline(bytes),
            },
            PreparedMetadataData::Object { media_type, object } => MetadataData::Media {
                media_type,
                data: MediaData::Object(object.key),
            },
        };
        Metadata {
            id,
            parent_binary_id,
            data,
            relation: self.relation,
            language: self.language,
            source: self.source,
        }
    }
}

impl PreparedMetadataData {
    fn database_values(&self) -> Result<[Value; 7], GameCatalogError> {
        let values =
            match self {
                Self::Text(value) => [
                    Value::from("text"),
                    Value::from(value.clone()),
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                ],
                Self::ReleaseYear(year) => [
                    Value::from("year"),
                    Value::Null,
                    Value::from(*year),
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                ],
                Self::Inline { media_type, bytes } => [
                    Value::from("inline"),
                    Value::Null,
                    Value::Null,
                    Value::from(bytes.to_vec()),
                    Value::Null,
                    Value::from(media_type.clone()),
                    Value::from(i64::try_from(bytes.len()).map_err(|_| {
                        GameCatalogError::InvalidData("media is too large".to_owned())
                    })?),
                ],
                Self::Object { media_type, object } => [
                    Value::from("object"),
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::from(object.digest.to_vec()),
                    Value::from(media_type.clone()),
                    Value::from(i64::try_from(object.byte_length).map_err(|_| {
                        GameCatalogError::InvalidData("media is too large".to_owned())
                    })?),
                ],
            };
        Ok(values)
    }
}
