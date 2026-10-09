use dashmap::DashMap;
use serde_json::Value;
use std::{path::Path, sync::Arc};
use tokio::sync::broadcast;
use turso::Value as SqlValue;
use uuid::Uuid;

use super::{Job, JobError, JobHandle, JobKind, JobSpec, JobState, JobStep, JobSubject};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS jobs (
    id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
    kind TEXT NOT NULL CHECK (length(kind) > 0),
    state TEXT NOT NULL CHECK (state IN ('queued', 'running', 'paused', 'completed', 'partial', 'failed', 'cancelled')),
    parent BLOB CHECK (parent IS NULL OR length(parent) = 16),
    dedupe TEXT,
    title TEXT NOT NULL,
    icon TEXT,
    subject TEXT NOT NULL,
    params TEXT NOT NULL,
    message TEXT,
    completed INTEGER,
    total INTEGER,
    detail TEXT,
    error TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS job_steps (
    job_id BLOB NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('queued', 'running', 'paused', 'completed', 'partial', 'failed', 'cancelled')),
    error TEXT,
    PRIMARY KEY (job_id, key)
) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS jobs_active ON jobs(kind, dedupe) WHERE dedupe IS NOT NULL AND state IN ('queued', 'running', 'paused');
CREATE INDEX IF NOT EXISTS jobs_created ON jobs(created_at DESC);
PRAGMA user_version = 2;
"#;

const COLUMNS: &str = "id, kind, state, parent, title, icon, subject, params, message, completed, total, detail, error, created_at, updated_at, dedupe";

/// Durable job records plus in-memory jobs that end with the process.
pub struct JobStore {
    database: turso::Database,
    ephemeral: DashMap<Uuid, Job>,
    ephemeral_dedupe: DashMap<(JobKind, String), Uuid>,
    pub(super) runners: DashMap<JobKind, Arc<dyn super::JobRunner>>,
    events: broadcast::Sender<Job>,
    /// Serializes writes, which the database rejects as busy when they overlap.
    writes: tokio::sync::Mutex<()>,
}

impl JobStore {
    pub async fn open(path: impl AsRef<Path>) -> Result<Arc<Self>, JobError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let database =
            turso::Builder::new_local(path.to_str().ok_or(JobError::InvalidDatabasePath)?)
                .build()
                .await?;
        let store = Self {
            database,
            ephemeral: DashMap::new(),
            ephemeral_dedupe: DashMap::new(),
            runners: DashMap::new(),
            events: broadcast::channel(256).0,
            writes: tokio::sync::Mutex::new(()),
        };
        let connection = store.connection().await?;
        let mut rows = connection.query("PRAGMA user_version", ()).await?;
        let version = rows
            .next()
            .await?
            .ok_or_else(|| JobError::InvalidData("missing schema version".to_owned()))?
            .get::<i64>(0)?;
        drop(rows);
        match version {
            0 => connection.execute_batch(SCHEMA).await?,
            1 => {
                connection
                    .execute_batch("DROP TABLE job_steps; DROP TABLE jobs;")
                    .await?;
                connection.execute_batch(SCHEMA).await?
            }
            2 => {}
            version => {
                return Err(JobError::InvalidData(format!(
                    "jobs database schema version {version} is not supported"
                )));
            }
        }
        let mut connection = store.connection().await?;
        let transaction = connection.transaction().await?;
        transaction.execute("UPDATE job_steps SET state = 'queued', error = NULL WHERE state = 'running' AND job_id IN (SELECT id FROM jobs WHERE state = 'running')", ()).await?;
        transaction
            .execute(
                "UPDATE jobs SET state = 'queued' WHERE state = 'running'",
                (),
            )
            .await?;
        transaction.commit().await?;
        store.abandon(JobKind::Download).await?;
        Ok(Arc::new(store))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Job> {
        self.events.subscribe()
    }

    /// Creates a queued job, or returns the active job with the same dedupe key.
    pub async fn enqueue(&self, spec: JobSpec) -> Result<(Job, bool), JobError> {
        let _write = self.writes.lock().await;
        let job = Job::new(Uuid::now_v7(), &spec, Self::now());
        if !spec.persistent {
            if let Some(dedupe) = spec.dedupe {
                let key = (spec.kind, dedupe);
                let active = self
                    .ephemeral_dedupe
                    .get(&key)
                    .and_then(|id| self.ephemeral.get(&*id).map(|job| job.clone()))
                    .filter(|existing| existing.state.active());
                if let Some(existing) = active {
                    return Ok((existing, false));
                }
                self.ephemeral_dedupe.insert(key, job.id);
            }
            self.ephemeral.insert(job.id, job.clone());
            self.publish(&job);
            return Ok((job, true));
        }
        let mut connection = self.connection().await?;
        let transaction = connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        if let Some(dedupe) = &spec.dedupe {
            let mut rows = transaction
                .query(
                    "SELECT id FROM jobs WHERE kind = ?1 AND dedupe = ?2 AND state IN ('queued', 'running', 'paused') LIMIT 1",
                    [SqlValue::from(spec.kind.as_str()), SqlValue::from(dedupe.as_str())],
                )
                .await?;
            if let Some(row) = rows.next().await? {
                let id = Self::uuid(row.get_value(0)?)?;
                drop(rows);
                transaction.commit().await?;
                let existing = self.get(id).await?.ok_or(JobError::NotFound(id))?;
                return Ok((existing, false));
            }
        }
        transaction
            .execute(
                &format!("INSERT INTO jobs ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)"),
                Self::row(&job),
            )
            .await?;
        Self::write_steps(&transaction, job.id, &job.steps).await?;
        transaction.commit().await?;
        self.publish(&job);
        Ok((job, true))
    }

    /// Moves a queued job to running and returns the handle that reports on it.
    pub async fn claim(self: &Arc<Self>, id: Uuid) -> Result<Option<JobHandle>, JobError> {
        let _write = self.writes.lock().await;
        let claimed = match self.ephemeral.get_mut(&id) {
            Some(mut job) if job.state == JobState::Queued => {
                job.state = JobState::Running;
                true
            }
            Some(_) => false,
            None => {
                self.connection()
                    .await?
                    .execute(
                        "UPDATE jobs SET state = 'running', updated_at = ?2 WHERE id = ?1 AND state = 'queued'",
                        vec![Self::id(id), SqlValue::from(Self::now())],
                    )
                    .await?
                    != 0
            }
        };
        if !claimed {
            return Ok(None);
        }
        if let Some(job) = self.get(id).await? {
            self.publish(&job);
        }
        Ok(Some(JobHandle::new(self.clone(), id)))
    }

    /// Creates a job and claims it immediately.
    pub async fn start(self: &Arc<Self>, spec: JobSpec) -> Result<JobHandle, JobError> {
        let (job, _) = self.enqueue(spec).await?;
        self.claim(job.id)
            .await?
            .ok_or(JobError::InvalidState(job.id, job.state))
    }

    pub async fn get(&self, id: Uuid) -> Result<Option<Job>, JobError> {
        if let Some(job) = self.ephemeral.get(&id) {
            return Ok(Some(job.clone()));
        }
        let connection = self.connection().await?;
        let mut rows = connection
            .query(
                &format!("SELECT {COLUMNS} FROM jobs WHERE id = ?1"),
                [Self::id(id)],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        let mut job = Self::job(&row)?;
        drop(rows);
        job.steps = self.steps(&connection, id).await?;
        Ok(Some(job))
    }

    pub async fn list(&self) -> Result<Vec<Job>, JobError> {
        let connection = self.connection().await?;
        let mut rows = connection
            .query(
                &format!("SELECT {COLUMNS} FROM jobs ORDER BY created_at DESC, id DESC"),
                (),
            )
            .await?;
        let mut jobs = self
            .ephemeral
            .iter()
            .map(|job| job.clone())
            .collect::<Vec<_>>();
        while let Some(row) = rows.next().await? {
            jobs.push(Self::job(&row)?);
        }
        drop(rows);
        for job in jobs.iter_mut().filter(|job| job.persistent) {
            job.steps = self.steps(&connection, job.id).await?;
        }
        jobs.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
        Ok(jobs)
    }

    pub(super) async fn queued(&self, kind: JobKind) -> Result<Vec<Job>, JobError> {
        Ok(self
            .list()
            .await?
            .into_iter()
            .filter(|job| job.kind == kind && job.state == JobState::Queued)
            .collect())
    }

    /// Makes a finished job queued again, unless another active job has taken its dedupe key.
    pub(super) async fn requeue(&self, id: Uuid) -> Result<Job, JobError> {
        let _write = self.writes.lock().await;
        let job = self.get(id).await?.ok_or(JobError::NotFound(id))?;
        if job.state.active() {
            return Err(JobError::InvalidState(id, job.state));
        }
        if let Some(dedupe) = &job.dedupe {
            if let Some(active) = self.active(job.kind, dedupe).await? {
                return Ok(active);
            }
            if !job.persistent {
                self.ephemeral_dedupe.insert((job.kind, dedupe.clone()), id);
            }
        }
        self.modify_locked(id, |job| {
            job.state = JobState::Queued;
            job.error = None;
            Ok(())
        })
        .await
    }

    async fn active(&self, kind: JobKind, dedupe: &str) -> Result<Option<Job>, JobError> {
        let ephemeral = self
            .ephemeral_dedupe
            .get(&(kind, dedupe.to_owned()))
            .and_then(|id| self.ephemeral.get(&*id).map(|job| job.clone()))
            .filter(|job| job.state.active());
        if ephemeral.is_some() {
            return Ok(ephemeral);
        }
        let connection = self.connection().await?;
        let mut rows = connection
            .query(
                &format!("SELECT {COLUMNS} FROM jobs WHERE kind = ?1 AND dedupe = ?2 AND state IN ('queued', 'running', 'paused') LIMIT 1"),
                [SqlValue::from(kind.as_str()), SqlValue::from(dedupe)],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        Self::job(&row).map(Some)
    }

    /// The most recent persistent job of `kind` created with `dedupe`, in any state.
    pub async fn latest(&self, kind: JobKind, dedupe: &str) -> Result<Option<Job>, JobError> {
        let connection = self.connection().await?;
        let mut rows = connection
            .query(
                &format!("SELECT {COLUMNS} FROM jobs WHERE kind = ?1 AND dedupe = ?2 ORDER BY created_at DESC, id DESC LIMIT 1"),
                [SqlValue::from(kind.as_str()), SqlValue::from(dedupe)],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        Self::job(&row).map(Some)
    }

    /// Waits until the job is no longer queued, running, or paused.
    pub async fn wait(&self, id: Uuid) -> Result<Job, JobError> {
        let mut changes = self.events.subscribe();
        loop {
            let job = self.get(id).await?.ok_or(JobError::NotFound(id))?;
            if !job.state.active() {
                return Ok(job);
            }
            loop {
                match changes.recv().await {
                    Ok(changed) if changed.id == id => break,
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => break,
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(JobError::InvalidData("job store closed".to_owned()));
                    }
                }
            }
        }
    }

    pub async fn cancel(&self, id: Uuid) -> Result<Job, JobError> {
        self.modify(id, |job| {
            if !matches!(job.state, JobState::Queued | JobState::Paused) {
                return Err(JobError::InvalidState(id, job.state));
            }
            job.state = JobState::Cancelled;
            Ok(())
        })
        .await
    }

    pub async fn dismiss(&self, id: Uuid) -> Result<(), JobError> {
        let _write = self.writes.lock().await;
        let job = self.get(id).await?.ok_or(JobError::NotFound(id))?;
        if job.state.active() {
            return Err(JobError::InvalidState(id, job.state));
        }
        if self.ephemeral.remove(&id).is_none() {
            self.connection()
                .await?
                .execute("DELETE FROM jobs WHERE id = ?1", [Self::id(id)])
                .await?;
        }
        Ok(())
    }

    pub(super) async fn modify(
        &self,
        id: Uuid,
        change: impl FnOnce(&mut Job) -> Result<(), JobError>,
    ) -> Result<Job, JobError> {
        let _write = self.writes.lock().await;
        self.modify_locked(id, change).await
    }

    async fn modify_locked(
        &self,
        id: Uuid,
        change: impl FnOnce(&mut Job) -> Result<(), JobError>,
    ) -> Result<Job, JobError> {
        if let Some(mut job) = self.ephemeral.get_mut(&id) {
            change(&mut job)?;
            job.updated_at = Self::now();
            let job = job.clone();
            self.publish(&job);
            return Ok(job);
        }
        let mut job = self.get(id).await?.ok_or(JobError::NotFound(id))?;
        change(&mut job)?;
        job.updated_at = Self::now();
        let mut connection = self.connection().await?;
        let transaction = connection.transaction().await?;
        transaction
            .execute(
                "UPDATE jobs SET state = ?3, title = ?5, icon = ?6, params = ?8, message = ?9, completed = ?10, total = ?11, detail = ?12, error = ?13, updated_at = ?15, dedupe = ?16 WHERE id = ?1",
                Self::row(&job),
            )
            .await?;
        Self::write_steps(&transaction, id, &job.steps).await?;
        transaction.commit().await?;
        self.publish(&job);
        Ok(job)
    }

    /// Fails an in-memory job without awaiting, returning false for persistent jobs.
    pub(super) fn interrupt_ephemeral(&self, id: Uuid) -> bool {
        let Some(mut job) = self.ephemeral.get_mut(&id) else {
            return false;
        };
        if job.state != JobState::Running {
            return true;
        }
        job.state = JobState::Failed;
        job.error = Some("Interrupted".to_owned());
        job.updated_at = Self::now();
        let job = job.clone();
        self.publish(&job);
        true
    }

    fn publish(&self, job: &Job) {
        let _ = self.events.send(job.clone());
    }

    async fn connection(&self) -> Result<turso::Connection, JobError> {
        let connection = self.database.connect()?;
        connection.execute("PRAGMA foreign_keys = ON", ()).await?;
        Ok(connection)
    }

    async fn steps(
        &self,
        connection: &turso::Connection,
        id: Uuid,
    ) -> Result<Vec<JobStep>, JobError> {
        let mut rows = connection
            .query(
                "SELECT key, state, error FROM job_steps WHERE job_id = ?1 ORDER BY ordinal",
                [Self::id(id)],
            )
            .await?;
        let mut steps = Vec::new();
        while let Some(row) = rows.next().await? {
            steps.push(JobStep {
                key: row.get(0)?,
                state: JobState::parse(&row.get::<String>(1)?)?,
                error: Self::text(row.get_value(2)?),
            });
        }
        Ok(steps)
    }

    async fn write_steps(
        transaction: &turso::transaction::Transaction<'_>,
        id: Uuid,
        steps: &[JobStep],
    ) -> Result<(), JobError> {
        for (ordinal, step) in steps.iter().enumerate() {
            transaction
                .execute(
                    "INSERT INTO job_steps (job_id, key, ordinal, state, error) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(job_id, key) DO UPDATE SET ordinal = excluded.ordinal, state = excluded.state, error = excluded.error",
                    vec![
                        Self::id(id),
                        SqlValue::from(step.key.as_str()),
                        SqlValue::from(ordinal as i64),
                        SqlValue::from(step.state.as_str()),
                        step.error.as_deref().map(SqlValue::from).unwrap_or(SqlValue::Null),
                    ],
                )
                .await?;
        }
        Ok(())
    }

    fn row(job: &Job) -> Vec<SqlValue> {
        let json = |value: &Value| serde_json::to_string(value).expect("job JSON is serializable");
        let text = |value: Option<&str>| value.map(SqlValue::from).unwrap_or(SqlValue::Null);
        let number = |value: Option<u64>| {
            value
                .map(|value| SqlValue::from(value as i64))
                .unwrap_or(SqlValue::Null)
        };
        vec![
            Self::id(job.id),
            SqlValue::from(job.kind.as_str()),
            SqlValue::from(job.state.as_str()),
            job.parent.map(Self::id).unwrap_or(SqlValue::Null),
            SqlValue::from(job.title.as_str()),
            text(job.icon.as_deref()),
            SqlValue::from(
                serde_json::to_string(&job.subject).expect("job subject is serializable"),
            ),
            SqlValue::from(json(&job.params)),
            text(job.message.as_deref()),
            number(job.completed),
            number(job.total),
            if job.detail.is_null() {
                SqlValue::Null
            } else {
                SqlValue::from(json(&job.detail))
            },
            text(job.error.as_deref()),
            SqlValue::from(job.created_at),
            SqlValue::from(job.updated_at),
            text(job.dedupe.as_deref()),
        ]
    }

    fn job(row: &turso::Row) -> Result<Job, JobError> {
        let json = |value: SqlValue| -> Result<Value, JobError> {
            Self::text(value)
                .map(|text| serde_json::from_str(&text))
                .transpose()
                .map(Option::unwrap_or_default)
                .map_err(|error| JobError::InvalidData(error.to_string()))
        };
        let number = |value: SqlValue| match value {
            SqlValue::Integer(value) => Some(value as u64),
            _ => None,
        };
        Ok(Job {
            id: Self::uuid(row.get_value(0)?)?,
            kind: JobKind::parse(&row.get::<String>(1)?)?,
            state: JobState::parse(&row.get::<String>(2)?)?,
            parent: match row.get_value(3)? {
                SqlValue::Null => None,
                value => Some(Self::uuid(value)?),
            },
            title: row.get(4)?,
            icon: Self::text(row.get_value(5)?),
            subject: serde_json::from_value::<JobSubject>(json(row.get_value(6)?)?)
                .map_err(|error| JobError::InvalidData(error.to_string()))?,
            params: json(row.get_value(7)?)?,
            message: Self::text(row.get_value(8)?),
            completed: number(row.get_value(9)?),
            total: number(row.get_value(10)?),
            detail: json(row.get_value(11)?)?,
            error: Self::text(row.get_value(12)?),
            persistent: true,
            created_at: row.get(13)?,
            updated_at: row.get(14)?,
            steps: Vec::new(),
            dedupe: Self::text(row.get_value(15)?),
        })
    }

    fn id(value: Uuid) -> SqlValue {
        SqlValue::from(value.as_bytes().to_vec())
    }

    fn uuid(value: SqlValue) -> Result<Uuid, JobError> {
        match value {
            SqlValue::Blob(bytes) => Uuid::from_slice(&bytes)
                .map_err(|error| JobError::InvalidData(format!("invalid UUID: {error}"))),
            _ => Err(JobError::InvalidData("expected a UUID blob".to_owned())),
        }
    }

    fn text(value: SqlValue) -> Option<String> {
        match value {
            SqlValue::Text(value) => Some(value),
            _ => None,
        }
    }

    pub(super) fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64
    }
}
