mod transfer;

pub(crate) use transfer::DownloadOptions;

use crate::{cache::CacheState, runtime::jobs::PluginJobs};
use deno_core::{OpState, ResourceId, op2};
use deno_error::JsErrorBox;
use polymer_library::jobs::{JobKind, JobOutcome, JobSpec, JobState, JobStore, JobSubject};
use std::{cell::RefCell, rc::Rc, sync::Arc};
use transfer::Transfer;

/// What a plugin's downloads may do, from its manifest.
#[derive(Clone)]
pub(crate) struct DownloadBinding {
    /// Allowed hosts whose certificates are not verified, as with the plugin's Fetch.
    pub insecure_hosts: Arc<[String]>,
}

impl DownloadBinding {
    fn client(&self, url: &str) -> Result<reqwest::Client, JsErrorBox> {
        let insecure = deno_core::url::Url::parse(url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .is_some_and(|host| self.insecure_hosts.contains(&host));
        reqwest::Client::builder()
            .tls_danger_accept_invalid_certs(insecure)
            .build()
            .map_err(|error| JsErrorBox::generic(error.to_string()))
    }
}

impl Transfer {
    /// Run the transfer as a persistent `download` job. A new job continues from the
    /// checkpoint of the file's previous job, so an interrupted or failed download resumes
    /// the next time the file is requested. A download of the same file already running is
    /// waited on instead of started again.
    async fn run_as_job(
        mut self,
        store: Arc<JobStore>,
        plugin: String,
        key: String,
    ) -> Result<(), JsErrorBox> {
        let failed =
            |error: polymer_library::jobs::JobError| JsErrorBox::generic(error.to_string());
        let title = self
            .path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let spec = JobSpec {
            kind: JobKind::Download,
            parent: self.parent,
            title,
            subject: JobSubject {
                plugin_id: Some(plugin.clone()),
                ..Default::default()
            },
            params: serde_json::json!({ "url": self.url, "file": key }),
            dedupe: Some(format!("{plugin}:{key}")),
            steps: Vec::new(),
            persistent: true,
        };
        let previous = store
            .latest(JobKind::Download, &format!("{plugin}:{key}"))
            .await
            .map_err(failed)?;
        let (job, created) = store.enqueue(spec).await.map_err(failed)?;
        let Some(handle) = store.claim(job.id).await.map_err(failed)? else {
            let done = store.wait(job.id).await.map_err(failed)?;
            return match done.state {
                JobState::Completed => Ok(()),
                _ => {
                    Err(JsErrorBox::generic(done.error.unwrap_or_else(|| {
                        format!("{} was not downloaded", done.title)
                    })))
                }
            };
        };
        if let Some(previous) =
            previous.filter(|previous| created && previous.state != JobState::Completed)
        {
            handle.record(previous.detail).await.map_err(failed)?;
        }
        let handle = Arc::new(handle);
        self.job = Some(handle.clone());
        let result = self.run().await;
        handle
            .settle(result.as_ref().map(|()| JobOutcome::completed(None)))
            .await
            .map_err(failed)?;
        result
    }
}

/// Download `url` into a file in one of the plugin's caches, as a job.
#[op2]
async fn op_download(
    state: Rc<RefCell<OpState>>,
    #[string] url: String,
    #[string] path: String,
    #[string] cache: String,
    #[serde] options: Option<DownloadOptions>,
    #[smi] parent: Option<ResourceId>,
) -> Result<(), JsErrorBox> {
    let (transfer, jobs) = {
        let mut state = state.borrow_mut();
        polymer_deno::Runtime::check_network(&mut state, &url, "download()")?;
        let jobs = state.try_borrow::<PluginJobs>().cloned();
        let transfer = Transfer {
            client: state.borrow::<DownloadBinding>().client(&url)?,
            path: state.borrow::<CacheState>().path(&cache, &path)?,
            url,
            options: options.unwrap_or_default(),
            job: None,
            parent: PluginJobs::parent(&state, parent),
        };
        (transfer, jobs)
    };
    match jobs.and_then(|jobs| Some((jobs.store?, jobs.plugin))) {
        Some((store, plugin)) => {
            transfer
                .run_as_job(store, plugin, format!("{cache}/{path}"))
                .await
        }
        None => transfer.run().await,
    }
}

deno_core::extension!(polymer_download,
    deps = [polymer_cache],
    ops = [op_download],
    esm_entry_point = "ext:polymer_download/00_download.js",
    esm = [dir "download/js", "00_download.js"],
    options = { binding: DownloadBinding },
    state = |state, options| state.put(options.binding),
);

pub(crate) fn extension(binding: DownloadBinding) -> deno_core::Extension {
    polymer_download::init(binding)
}
