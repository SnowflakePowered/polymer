use deno_error::JsErrorBox;
use polymer_library::{
    digest::DigestAlgorithm,
    jobs::{JobHandle, JobProgress},
};
use reqwest::{StatusCode, header};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
    sync::Arc,
};
use uuid::Uuid;

const CHECKPOINT_INTERVAL: u64 = 8 * 1024 * 1024;

/// What a plugin knows about a file before downloading it. A file is verified against
/// whatever is given; with nothing given, a completed transfer is accepted as is.
#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DownloadOptions {
    byte_length: Option<u64>,
    sha1: Option<String>,
    sha256: Option<String>,
    /// The digest covers only the file's first `digest_length` bytes.
    digest_length: Option<u64>,
    /// The transfer fails once the file grows past this many bytes.
    max_byte_length: Option<u64>,
}

/// How far an earlier attempt got, recorded in the download job's `detail.checkpoint`.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Checkpoint {
    url: String,
    etag: Option<String>,
    last_modified: Option<String>,
    completed: u64,
}

/// A resumable, verified transfer of one URL into a plugin cache file.
pub(super) struct Transfer {
    pub client: reqwest::Client,
    pub url: String,
    pub path: PathBuf,
    pub options: DownloadOptions,
    /// The download's own job, which holds its checkpoint.
    pub job: Option<Arc<JobHandle>>,
    /// The job that requested the download.
    pub parent: Option<Uuid>,
}

impl DownloadOptions {
    fn digest(&self) -> Option<(DigestAlgorithm, &str)> {
        match (&self.sha256, &self.sha1) {
            (Some(sha256), _) => Some((DigestAlgorithm::Sha2, sha256)),
            (None, Some(sha1)) => Some((DigestAlgorithm::Sha1, sha1)),
            (None, None) => None,
        }
    }

    /// Whether a file can be checked for completeness without downloading it.
    fn checkable(&self) -> bool {
        self.byte_length.is_some() || self.digest().is_some()
    }

    /// Whether `path` holds the complete, expected file.
    fn verify(&self, path: &PathBuf) -> std::io::Result<bool> {
        let Ok(file) = fs::File::open(path) else {
            return Ok(false);
        };
        let length = file.metadata()?.len();
        let covered = self.digest_length.unwrap_or(length);
        if self.byte_length.is_some_and(|expected| expected != length) || covered > length {
            return Ok(false);
        }
        let Some((algorithm, expected)) = self.digest() else {
            return Ok(true);
        };
        Ok(algorithm
            .hash(file.take(covered))?
            .eq_ignore_ascii_case(expected))
    }
}

impl Transfer {
    /// Download the file unless it is already complete, resuming an earlier attempt.
    /// A resumed file that fails verification is downloaded once more from the start.
    pub(super) async fn run(&self) -> Result<(), JsErrorBox> {
        let checkable = self.options.checkable();
        if checkable && self.verified().await? {
            return Ok(());
        }
        let resume = self.checkpoint().await.filter(|checkpoint| {
            checkpoint.url == self.url
                && checkpoint.completed > 0
                && fs::metadata(&self.path)
                    .is_ok_and(|metadata| metadata.len() >= checkpoint.completed)
        });
        let resumed = resume.is_some();
        self.transfer(resume).await?;
        if !checkable || self.verified().await? {
            return Ok(());
        }
        if resumed {
            self.transfer(None).await?;
            if self.verified().await? {
                return Ok(());
            }
        }
        Err(JsErrorBox::generic(format!(
            "{} does not match its expected size and digest",
            self.url
        )))
    }

    async fn verified(&self) -> Result<bool, JsErrorBox> {
        let (options, path) = (self.options.clone(), self.path.clone());
        tokio::task::spawn_blocking(move || options.verify(&path))
            .await
            .map_err(|error| JsErrorBox::generic(error.to_string()))?
            .map_err(|error| JsErrorBox::generic(error.to_string()))
    }

    /// Request the file, continuing from `resume` when the server still has the same copy.
    async fn transfer(&self, resume: Option<Checkpoint>) -> Result<(), JsErrorBox> {
        let mut request = self.client.get(&self.url);
        if let Some(resume) = &resume {
            request = request.header(header::RANGE, format!("bytes={}-", resume.completed));
            if let Some(validator) = resume.etag.as_ref().or(resume.last_modified.as_ref()) {
                request = request.header(header::IF_RANGE, validator);
            }
        }
        let mut response = request
            .send()
            .await
            .map_err(|error| JsErrorBox::generic(error.to_string()))?;
        let start = match (response.status(), &resume) {
            (StatusCode::PARTIAL_CONTENT, Some(resume)) => resume.completed,
            (StatusCode::OK, _) => 0,
            (status, _) => {
                return Err(JsErrorBox::generic(format!(
                    "{} returned HTTP {status}",
                    self.url
                )));
            }
        };
        let text = |name| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        let mut checkpoint = Checkpoint {
            url: self.url.clone(),
            etag: text(header::ETAG),
            last_modified: text(header::LAST_MODIFIED),
            completed: start,
        };
        let total = response
            .content_length()
            .map(|length| start + length)
            .or(self.options.byte_length);
        let maximum = self.options.max_byte_length.unwrap_or(u64::MAX);
        let oversized = || JsErrorBox::generic(format!("{} exceeds the download limit", self.url));
        if total.is_some_and(|total| total > maximum) {
            return Err(oversized());
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|error| JsErrorBox::generic(error.to_string()))?;
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&self.path)
            .map_err(|error| JsErrorBox::generic(error.to_string()))?;
        file.set_len(start)
            .map_err(|error| JsErrorBox::generic(error.to_string()))?;
        file.seek(SeekFrom::Start(start))
            .map_err(|error| JsErrorBox::generic(error.to_string()))?;
        self.save(&checkpoint).await?;
        let mut saved = start;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| JsErrorBox::generic(error.to_string()))?
        {
            file.write_all(&chunk)
                .map_err(|error| JsErrorBox::generic(error.to_string()))?;
            checkpoint.completed += chunk.len() as u64;
            if checkpoint.completed > maximum {
                return Err(oversized());
            }
            self.report(checkpoint.completed, total).await;
            if checkpoint.completed - saved >= CHECKPOINT_INTERVAL {
                file.flush()
                    .map_err(|error| JsErrorBox::generic(error.to_string()))?;
                self.save(&checkpoint).await?;
                saved = checkpoint.completed;
            }
        }
        file.flush()
            .map_err(|error| JsErrorBox::generic(error.to_string()))?;
        self.save(&checkpoint).await
    }

    async fn report(&self, completed: u64, total: Option<u64>) {
        let name = self.path.file_name().unwrap_or_default().to_string_lossy();
        if let Some(job) = &self.job {
            let progress = JobProgress {
                message: Some(format!("Downloading {name}")),
                completed: Some(completed),
                total,
                ..Default::default()
            };
            if let Err(error) = job.progress(progress).await {
                tracing::warn!(%error, job = %job.id(), "could not record download progress");
            }
        }
    }

    async fn checkpoint(&self) -> Option<Checkpoint> {
        let detail = self.job.as_ref()?.job().await.ok()?.detail;
        serde_json::from_value(detail["checkpoint"].clone()).ok()
    }

    async fn save(&self, checkpoint: &Checkpoint) -> Result<(), JsErrorBox> {
        let Some(job) = &self.job else {
            return Ok(());
        };
        let mut detail = job
            .job()
            .await
            .map_err(|error| JsErrorBox::generic(error.to_string()))?
            .detail;
        detail["checkpoint"] = serde_json::json!(checkpoint);
        job.record(detail)
            .await
            .map_err(|error| JsErrorBox::generic(error.to_string()))?;
        Ok(())
    }
}
