use super::LibraryBackend;
use kameo::message::{Context, Message};
use polymer_deno::polymer::library::*;
use serde_json::{Value, json};

impl Message<ScrapingJobs> for LibraryBackend {
    type Reply = Result<Value, String>;

    async fn handle(
        &mut self,
        _message: ScrapingJobs,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.catalog
            .scrape_jobs()
            .await
            .map(|jobs| json!(jobs))
            .map_err(|error| error.to_string())
    }
}

impl Message<ScrapingStart> for LibraryBackend {
    type Reply = Result<Value, String>;

    async fn handle(
        &mut self,
        ScrapingStart {
            root_binary_id,
            trigger_binary_id,
        }: ScrapingStart,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.scraping
            .enqueue(
                Self::uuid(&root_binary_id)?,
                Self::uuid(&trigger_binary_id)?,
            )
            .await
            .map(|job| json!(job))
            .map_err(|error| error.to_string())
    }
}

impl Message<ScrapingJob> for LibraryBackend {
    type Reply = Result<Value, String>;

    async fn handle(
        &mut self,
        ScrapingJob { id }: ScrapingJob,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.catalog
            .scrape_job(Self::uuid(&id)?)
            .await
            .map(|job| json!(job))
            .map_err(|error| error.to_string())
    }
}

impl Message<ScrapingRetry> for LibraryBackend {
    type Reply = Result<Value, String>;

    async fn handle(
        &mut self,
        ScrapingRetry { id }: ScrapingRetry,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.scraping
            .retry(Self::uuid(&id)?)
            .await
            .map(|job| json!(job))
            .map_err(|error| error.to_string())
    }
}

impl Message<ScrapingScrapers> for LibraryBackend {
    type Reply = Result<Value, String>;

    async fn handle(
        &mut self,
        _message: ScrapingScrapers,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        Ok(Value::Array(self.scraping.scrapers().iter().map(|plugin| json!({
            "id": plugin.id(),
            "version": plugin.version(),
            "requires": plugin.requirements().iter().map(ToString::to_string).collect::<Vec<_>>(),
            "provides": plugin.provides(),
        })).collect()))
    }
}

impl Message<ScrapingRefresh> for LibraryBackend {
    type Reply = Result<(), String>;

    async fn handle(
        &mut self,
        ScrapingRefresh { plugin }: ScrapingRefresh,
        _: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.scraping
            .refresh(&plugin)
            .await
            .map_err(|error| error.to_string())
    }
}
