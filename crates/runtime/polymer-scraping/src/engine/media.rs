use super::{ScrapingEngine, ScrapingError};
use futures_util::{StreamExt, TryStreamExt};
use polymer_library::{MetadataInput, MetadataInputData, MetadataValue};
use polymer_pluginhost::scraper::{InlineMedia, ScrapeResult, ScrapeValue};
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;

const MAX_CONCURRENT_MEDIA_DOWNLOADS: usize = 4;

impl ScrapingEngine {
    pub(super) async fn stage_media(
        &self,
        results: Vec<ScrapeResult>,
        directory: &std::path::Path,
    ) -> Result<Vec<MetadataInput>, ScrapingError> {
        let mut inputs = std::iter::repeat_with(|| None)
            .take(results.len())
            .collect::<Vec<_>>();
        let mut media = Vec::new();
        for (index, result) in results.into_iter().enumerate() {
            let data = match result.value {
                ScrapeValue::Inline(value) => MetadataInputData::Inline {
                    media_type: value.media_type,
                    bytes: value.bytes,
                    source_url: None,
                },
                ScrapeValue::Scalar(MetadataValue::String(url))
                    if result.key.starts_with("media.") =>
                {
                    media.push((index, result.key, result.source, url));
                    continue;
                }
                ScrapeValue::Scalar(value) => MetadataInputData::Scalar { value },
            };
            inputs[index] = Some(MetadataInput {
                key: result.key,
                source: result.source,
                data,
            });
        }
        let temporary_path = directory.to_owned();
        let downloaded = futures_util::stream::iter(media)
            .map(|(index, key, source, url)| {
                let directory = temporary_path.clone();
                async move {
                    let (path, media_type) = self.download(&key, &url, &directory, index).await?;
                    Ok::<_, ScrapingError>((index, key, source, url, path, media_type))
                }
            })
            .buffer_unordered(MAX_CONCURRENT_MEDIA_DOWNLOADS)
            .try_collect::<Vec<_>>()
            .await?;
        for (index, key, source, url, path, media_type) in downloaded {
            inputs[index] = Some(MetadataInput {
                key,
                source,
                data: MetadataInputData::Object {
                    media_type,
                    path,
                    source_url: Some(url),
                },
            });
        }
        Ok(inputs
            .into_iter()
            .map(|value| value.expect("every scrape result was staged"))
            .collect())
    }

    async fn download(
        &self,
        key: &str,
        url: &str,
        directory: &std::path::Path,
        index: usize,
    ) -> Result<(PathBuf, String), ScrapingError> {
        let response = self
            .inner
            .client
            .get(url)
            .send()
            .await?
            .error_for_status()?;
        let media_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .filter(|value| InlineMedia::accepts_type(key, value))
            .ok_or_else(|| {
                ScrapingError::InvalidMedia(
                    "media response has no supported Content-Type".to_owned(),
                )
            })?
            .to_owned();
        let path = directory.join(format!("media-{index}"));
        let mut output = tokio::fs::File::create(&path).await?;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            output.write_all(&chunk).await?;
        }
        output.flush().await?;
        Ok((path, media_type))
    }
}
