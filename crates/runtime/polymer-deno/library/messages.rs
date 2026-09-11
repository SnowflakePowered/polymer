use kameo::actor::ReplyRecipient;
use serde_json::Value;

/// Kameo recipients exposed to Deno without depending on the runtime backend.
#[derive(Clone, Debug)]
pub struct Library {
    pub games_list: ReplyRecipient<GamesList, Value, String>,
    pub games_get: ReplyRecipient<GamesGet, Value, String>,
    pub games_delete: ReplyRecipient<GamesDelete, (), String>,
    pub launchables_add: ReplyRecipient<LaunchablesAdd, Value, String>,
    pub launchables_delete: ReplyRecipient<LaunchablesDelete, (), String>,
    pub metadata_add: ReplyRecipient<MetadataAdd, Value, String>,
    pub metadata_update: ReplyRecipient<MetadataUpdate, Value, String>,
    pub metadata_delete: ReplyRecipient<MetadataDelete, (), String>,
    pub metadata_replace: ReplyRecipient<MetadataReplace, Value, String>,
    pub metadata_text: ReplyRecipient<MetadataText, String, String>,
    pub metadata_url: ReplyRecipient<MetadataUrl, String, String>,
    pub scraping_jobs: ReplyRecipient<ScrapingJobs, Value, String>,
    pub scraping_start: ReplyRecipient<ScrapingStart, Value, String>,
    pub scraping_job: ReplyRecipient<ScrapingJob, Value, String>,
    pub scraping_retry: ReplyRecipient<ScrapingRetry, Value, String>,
    pub scraping_scrapers: ReplyRecipient<ScrapingScrapers, Value, String>,
    pub scraping_refresh: ReplyRecipient<ScrapingRefresh, (), String>,
}

#[derive(Debug)]
pub struct GamesList {
    pub platform: Option<String>,
}

#[derive(Debug)]
pub struct GamesGet {
    pub id: String,
}

#[derive(Debug)]
pub struct GamesDelete {
    pub id: String,
}

#[derive(Debug)]
pub struct LaunchablesAdd {
    pub path: String,
    pub platform: Option<String>,
    pub parent_binary_id: Option<String>,
}

#[derive(Debug)]
pub struct LaunchablesDelete {
    pub id: String,
}

#[derive(Debug)]
pub struct MetadataAdd {
    pub parent_binary_id: String,
    pub input: Value,
}

#[derive(Debug)]
pub struct MetadataUpdate {
    pub id: String,
    pub input: Value,
}

#[derive(Debug)]
pub struct MetadataDelete {
    pub id: String,
}

#[derive(Debug)]
pub struct MetadataReplace {
    pub parent_binary_id: String,
    pub source: String,
    pub inputs: Vec<Value>,
}

#[derive(Debug)]
pub struct MetadataText {
    pub id: String,
}

#[derive(Debug)]
pub struct MetadataUrl {
    pub id: String,
}

#[derive(Debug)]
pub struct ScrapingJobs;

#[derive(Debug)]
pub struct ScrapingStart {
    pub root_binary_id: String,
    pub trigger_binary_id: String,
}

#[derive(Debug)]
pub struct ScrapingJob {
    pub id: String,
}

#[derive(Debug)]
pub struct ScrapingRetry {
    pub id: String,
}

#[derive(Debug)]
pub struct ScrapingScrapers;

#[derive(Debug)]
pub struct ScrapingRefresh {
    pub plugin: String,
}
