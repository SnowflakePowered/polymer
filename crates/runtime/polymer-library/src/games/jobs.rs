use super::{GameCatalog, GameCatalogError};
use turso::Value;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ScrapeJobState {
    Queued,
    Running,
    Completed,
    Partial,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ScrapeAttemptState {
    Queued,
    Running,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrapeAttempt {
    pub plugin_id: String,
    pub state: ScrapeAttemptState,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrapeJob {
    #[serde(serialize_with = "serialize_uuid")]
    pub id: Uuid,
    #[serde(serialize_with = "serialize_uuid")]
    pub root_binary_id: Uuid,
    #[serde(serialize_with = "serialize_uuid")]
    pub trigger_binary_id: Uuid,
    pub state: ScrapeJobState,
    pub created_at: i64,
    pub updated_at: i64,
    pub attempts: Vec<ScrapeAttempt>,
}

fn serialize_uuid<S>(value: &Uuid, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&value.simple().to_string())
}

impl ScrapeJobState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Partial => "partial",
            Self::Failed => "failed",
        }
    }
    fn parse(value: &str) -> Result<Self, GameCatalogError> {
        Ok(match value {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "completed" => Self::Completed,
            "partial" => Self::Partial,
            "failed" => Self::Failed,
            _ => {
                return Err(GameCatalogError::InvalidData(format!(
                    "unknown scrape job state '{value}'"
                )));
            }
        })
    }
}

impl ScrapeAttemptState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
    fn parse(value: &str) -> Result<Self, GameCatalogError> {
        Ok(match value {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            _ => {
                return Err(GameCatalogError::InvalidData(format!(
                    "unknown scrape attempt state '{value}'"
                )));
            }
        })
    }
}

impl GameCatalog {
    pub async fn queue_scrape(
        &self,
        root: Uuid,
        trigger: Uuid,
        plugins: &[String],
    ) -> Result<(ScrapeJob, bool), GameCatalogError> {
        let mut connection = self.connection().await?;
        let transaction = connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        self.require_root(&transaction, root).await?;
        let mut trigger_rows = transaction
            .query(
                "SELECT COALESCE(parent_binary_id, id) FROM binaries WHERE id = ?1",
                [Self::id(trigger)],
            )
            .await?;
        let trigger_root = trigger_rows
            .next()
            .await?
            .ok_or_else(|| GameCatalogError::NotFound(trigger.simple().to_string()))
            .and_then(|row| Self::uuid(row.get_value(0)?))?;
        drop(trigger_rows);
        if trigger_root != root {
            return Err(GameCatalogError::InvalidData(
                "scrape trigger does not belong to the root game".to_owned(),
            ));
        }
        let mut existing = transaction.query("SELECT id FROM scrape_jobs WHERE root_binary_id = ?1 AND state IN ('queued', 'running') LIMIT 1", [Self::id(root)]).await?;
        if let Some(row) = existing.next().await? {
            let id = Self::uuid(row.get_value(0)?)?;
            drop(existing);
            transaction.commit().await?;
            return self
                .scrape_job(id)
                .await?
                .map(|job| (job, false))
                .ok_or_else(|| GameCatalogError::NotFound(id.simple().to_string()));
        }
        drop(existing);
        let id = Uuid::now_v7();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        transaction.execute("INSERT INTO scrape_jobs (id, root_binary_id, trigger_binary_id, state, created_at, updated_at) VALUES (?1, ?2, ?3, 'queued', ?4, ?4)", vec![Self::id(id), Self::id(root), Self::id(trigger), Value::from(now)]).await?;
        for plugin in plugins {
            transaction.execute("INSERT INTO scrape_attempts (job_id, plugin_id, state) VALUES (?1, ?2, 'queued')", vec![Self::id(id), Value::from(plugin.clone())]).await?;
        }
        transaction.commit().await?;
        self.scrape_job(id)
            .await?
            .map(|job| (job, true))
            .ok_or_else(|| GameCatalogError::NotFound(id.simple().to_string()))
    }

    pub async fn claim_scrape_job(&self, id: Uuid) -> Result<bool, GameCatalogError> {
        let connection = self.connection().await?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        Ok(connection
            .execute(
                "UPDATE scrape_jobs SET state = 'running', updated_at = ?2 WHERE id = ?1 AND state = 'queued'",
                vec![Self::id(id), Value::from(now)],
            )
            .await?
            != 0)
    }

    pub async fn scrape_jobs(&self) -> Result<Vec<ScrapeJob>, GameCatalogError> {
        let connection = self.connection().await?;
        let mut rows = connection
            .query(
                "SELECT id FROM scrape_jobs ORDER BY created_at DESC, id DESC",
                (),
            )
            .await?;
        let mut ids = Vec::new();
        while let Some(row) = rows.next().await? {
            ids.push(Self::uuid(row.get_value(0)?)?);
        }
        drop(rows);
        let mut jobs = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(job) = self.scrape_job(id).await? {
                jobs.push(job);
            }
        }
        Ok(jobs)
    }

    pub async fn scrape_job(&self, id: Uuid) -> Result<Option<ScrapeJob>, GameCatalogError> {
        let connection = self.connection().await?;
        let mut rows = connection.query("SELECT root_binary_id, trigger_binary_id, state, created_at, updated_at FROM scrape_jobs WHERE id = ?1", [Self::id(id)]).await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let root_binary_id = Self::uuid(row.get_value(0)?)?;
        let trigger_binary_id = Self::uuid(row.get_value(1)?)?;
        let state = ScrapeJobState::parse(&row.get::<String>(2)?)?;
        let created_at = row.get(3)?;
        let updated_at = row.get(4)?;
        drop(rows);
        let mut rows = connection.query("SELECT plugin_id, state, error FROM scrape_attempts WHERE job_id = ?1 ORDER BY plugin_id", [Self::id(id)]).await?;
        let mut attempts = Vec::new();
        while let Some(row) = rows.next().await? {
            attempts.push(ScrapeAttempt {
                plugin_id: row.get(0)?,
                state: ScrapeAttemptState::parse(&row.get::<String>(1)?)?,
                error: Self::optional_text(row.get_value(2)?)?,
            });
        }
        Ok(Some(ScrapeJob {
            id,
            root_binary_id,
            trigger_binary_id,
            state,
            created_at,
            updated_at,
            attempts,
        }))
    }

    pub async fn set_scrape_job_state(
        &self,
        id: Uuid,
        state: ScrapeJobState,
    ) -> Result<(), GameCatalogError> {
        let connection = self.connection().await?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let changed = connection
            .execute(
                "UPDATE scrape_jobs SET state = ?2, updated_at = ?3 WHERE id = ?1",
                vec![Self::id(id), Value::from(state.as_str()), Value::from(now)],
            )
            .await?;
        if changed == 0 {
            return Err(GameCatalogError::NotFound(id.simple().to_string()));
        }
        Ok(())
    }

    pub async fn set_scrape_attempt(
        &self,
        job: Uuid,
        plugin: &str,
        state: ScrapeAttemptState,
        error: Option<&str>,
    ) -> Result<(), GameCatalogError> {
        let connection = self.connection().await?;
        let changed = connection.execute("UPDATE scrape_attempts SET state = ?3, error = ?4 WHERE job_id = ?1 AND plugin_id = ?2", vec![Self::id(job), Value::from(plugin), Value::from(state.as_str()), error.map(Value::from).unwrap_or(Value::Null)]).await?;
        if changed == 0 {
            return Err(GameCatalogError::NotFound(format!(
                "{}:{plugin}",
                job.simple()
            )));
        }
        Ok(())
    }

    pub async fn recover_scrape_jobs(&self) -> Result<Vec<ScrapeJob>, GameCatalogError> {
        let connection = self.connection().await?;
        connection.execute_batch("UPDATE scrape_jobs SET state = 'queued' WHERE state = 'running'; UPDATE scrape_attempts SET state = 'queued', error = NULL WHERE state = 'running';").await?;
        Ok(self
            .scrape_jobs()
            .await?
            .into_iter()
            .filter(|job| job.state == ScrapeJobState::Queued)
            .collect())
    }
}
