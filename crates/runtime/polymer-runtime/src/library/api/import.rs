use super::LibraryBackend;
use actix::{AtomicResponse, Context, Handler};
use polymer_deno::polymer::library::LibraryImport;
use polymer_scraping::Ingestor;
use serde_json::{Value, json};
use std::path::PathBuf;

impl Handler<LibraryImport> for LibraryBackend {
    type Result = AtomicResponse<Self, Result<Value, String>>;

    fn handle(
        &mut self,
        LibraryImport { path }: LibraryImport,
        _: &mut Context<Self>,
    ) -> Self::Result {
        let scraping = self.scraping.clone();
        let firmware = self.firmware_runtime.handle();
        self.mutate(async move {
            let path = PathBuf::from(path);
            let identification_path = path.clone();
            let detected = tokio::task::spawn_blocking(move || Ingestor::identify(&identification_path, None, None))
                .await.map_err(|error| format!("package detection task failed: {error}"))?;
            match detected {
                Ok(packages) => Ok(json!({ "kind": "games", "imports": Self::import_games(&scraping, packages).await? })),
                Err(error) if error.firmware_candidate() && path.is_file() => {
                    match firmware.install_firmware(&path).await {
                        Ok(Some(installed)) => Ok(json!({ "kind": "firmware", "firmware": installed })),
                        Ok(None) => Err(format!("{error}; no platform recognized this file as firmware")),
                        Err(firmware_error) => Err(format!("{error}; {firmware_error}")),
                    }
                }
                Err(error) => Err(error.to_string()),
            }
        })
    }
}
