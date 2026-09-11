use actix::Recipient;
use serde_json::Value;

/// Actix recipients exposed to Deno without depending on the runtime backend.
#[derive(Clone, Debug)]
pub struct Library {
    pub games_list: Recipient<GamesList>,
    pub games_get: Recipient<GamesGet>,
    pub games_delete: Recipient<GamesDelete>,
    pub launchables_add: Recipient<LaunchablesAdd>,
    pub launchables_delete: Recipient<LaunchablesDelete>,
    pub metadata_add: Recipient<MetadataAdd>,
    pub metadata_update: Recipient<MetadataUpdate>,
    pub metadata_delete: Recipient<MetadataDelete>,
    pub metadata_replace: Recipient<MetadataReplace>,
    pub metadata_text: Recipient<MetadataText>,
    pub metadata_url: Recipient<MetadataUrl>,
    pub scraping_jobs: Recipient<ScrapingJobs>,
    pub scraping_start: Recipient<ScrapingStart>,
    pub scraping_job: Recipient<ScrapingJob>,
    pub scraping_retry: Recipient<ScrapingRetry>,
    pub scraping_scrapers: Recipient<ScrapingScrapers>,
    pub scraping_refresh: Recipient<ScrapingRefresh>,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<Value, String>")]
pub struct GamesList {
    pub platform: Option<String>,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<Value, String>")]
pub struct GamesGet {
    pub id: String,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<(), String>")]
pub struct GamesDelete {
    pub id: String,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<Value, String>")]
pub struct LaunchablesAdd {
    pub path: String,
    pub platform: Option<String>,
    pub parent_binary_id: Option<String>,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<(), String>")]
pub struct LaunchablesDelete {
    pub id: String,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<Value, String>")]
pub struct MetadataAdd {
    pub parent_binary_id: String,
    pub input: Value,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<Value, String>")]
pub struct MetadataUpdate {
    pub id: String,
    pub input: Value,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<(), String>")]
pub struct MetadataDelete {
    pub id: String,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<Value, String>")]
pub struct MetadataReplace {
    pub parent_binary_id: String,
    pub source: String,
    pub inputs: Vec<Value>,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<String, String>")]
pub struct MetadataText {
    pub id: String,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<String, String>")]
pub struct MetadataUrl {
    pub id: String,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<Value, String>")]
pub struct ScrapingJobs;

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<Value, String>")]
pub struct ScrapingStart {
    pub root_binary_id: String,
    pub trigger_binary_id: String,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<Value, String>")]
pub struct ScrapingJob {
    pub id: String,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<Value, String>")]
pub struct ScrapingRetry {
    pub id: String,
}

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<Value, String>")]
pub struct ScrapingScrapers;

#[derive(Debug, actix::Message)]
#[rtype(result = "Result<(), String>")]
pub struct ScrapingRefresh {
    pub plugin: String,
}
