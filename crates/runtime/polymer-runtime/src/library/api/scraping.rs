use super::{LibraryApi, LibraryBackend};
use crate::library::LibraryReader;
use actix::{AtomicResponse, Context, Handler, ResponseActFuture};
use polymer_deno::polymer::library::*;
use serde_json::{Value, json};

impl Handler<ScrapingJobs> for LibraryReader {
    type Result = ResponseActFuture<Self, Result<Value, String>>;

    fn handle(&mut self, _message: ScrapingJobs, _: &mut Context<Self>) -> Self::Result {
        let catalog = self.catalog.clone();
        self.read(async move {
            catalog
                .scrape_jobs()
                .await
                .map(|jobs| json!(jobs))
                .map_err(|error| error.to_string())
        })
    }
}

impl Handler<ScrapingStart> for LibraryBackend {
    type Result = AtomicResponse<Self, Result<Value, String>>;

    fn handle(
        &mut self,
        ScrapingStart {
            root_binary_id,
            trigger_binary_id,
        }: ScrapingStart,
        _: &mut Context<Self>,
    ) -> Self::Result {
        let scraping = self.scraping.clone();
        self.mutate(async move {
            scraping
                .enqueue(
                    Self::uuid(&root_binary_id)?,
                    Self::uuid(&trigger_binary_id)?,
                )
                .await
                .map(|job| json!(job))
                .map_err(|error| error.to_string())
        })
    }
}

impl Handler<ScrapingJob> for LibraryReader {
    type Result = ResponseActFuture<Self, Result<Value, String>>;

    fn handle(&mut self, ScrapingJob { id }: ScrapingJob, _: &mut Context<Self>) -> Self::Result {
        let catalog = self.catalog.clone();
        self.read(async move {
            catalog
                .scrape_job(Self::uuid(&id)?)
                .await
                .map(|job| json!(job))
                .map_err(|error| error.to_string())
        })
    }
}

impl Handler<ScrapingRetry> for LibraryBackend {
    type Result = AtomicResponse<Self, Result<Value, String>>;

    fn handle(
        &mut self,
        ScrapingRetry { id }: ScrapingRetry,
        _: &mut Context<Self>,
    ) -> Self::Result {
        let scraping = self.scraping.clone();
        self.mutate(async move {
            scraping
                .retry(Self::uuid(&id)?)
                .await
                .map(|job| json!(job))
                .map_err(|error| error.to_string())
        })
    }
}

impl Handler<ScrapingScrapers> for LibraryReader {
    type Result = Result<Value, String>;

    fn handle(&mut self, _: ScrapingScrapers, _: &mut Context<Self>) -> Self::Result {
        Ok(Value::Array(self.scraping.scrapers().iter().map(|plugin| json!({
            "id": plugin.id(),
            "version": plugin.version(),
            "requires": plugin.requirements().iter().map(ToString::to_string).collect::<Vec<_>>(),
            "provides": plugin.provides(),
        })).collect()))
    }
}

impl Handler<ScrapingRefresh> for LibraryBackend {
    type Result = AtomicResponse<Self, Result<(), String>>;

    fn handle(
        &mut self,
        ScrapingRefresh { plugin }: ScrapingRefresh,
        _: &mut Context<Self>,
    ) -> Self::Result {
        let scraping = self.scraping.clone();
        self.mutate(async move {
            scraping
                .refresh(&plugin)
                .await
                .map_err(|error| error.to_string())
        })
    }
}
