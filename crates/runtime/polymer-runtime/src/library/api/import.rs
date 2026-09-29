use super::LibraryBackend;
use actix::{AtomicResponse, Context, Handler};
use polymer_consolestorage::keys::{KeyIdentity, KeyRecord, KeyWriter};
use polymer_deno::polymer::library::{ImportedKey, LibraryImport, LibraryImportKey};
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
        let firmware_root = self.firmware_root.clone();
        self.mutate(async move {
            let path = PathBuf::from(path);
            let identification_path = path.clone();
            let detected = tokio::task::spawn_blocking(move || Ingestor::identify(&identification_path, None, None))
                .await.map_err(|error| format!("package detection task failed: {error}"))?;
            match detected {
                Ok(packages) if packages.iter().any(|package| package.installable) => Ok(json!({
                    "kind": "installables",
                    "installables": packages.iter().map(|package| json!({
                        "path": package.uri.host_path().ok(),
                        "filename": package.filename,
                        "platform": package.platform,
                        "titleId": package.title_id,
                    })).collect::<Vec<_>>(),
                })),
                Ok(packages) => Ok(json!({ "kind": "games", "imports": Self::import_games(&scraping, packages).await? })),
                Err(error) if error.firmware_candidate() && path.is_file() => {
                    let firmware = polymer_library::firmware::FirmwareStore::open(firmware_root).await.map_err(|error| error.to_string())?;
                    match firmware.install_bios(&path).await {
                        Ok(Some(installed)) => Ok(json!({ "kind": "firmware", "firmware": installed })),
                        Ok(None) => Err(format!("{error}; no consolespec BIOS hash matched this file")),
                        Err(firmware_error) => Err(format!("{error}; {firmware_error}")),
                    }
                }
                Err(error) => Err(error.to_string()),
            }
        })
    }
}

impl Handler<LibraryImportKey> for LibraryBackend {
    type Result = AtomicResponse<Self, Result<ImportedKey, String>>;

    fn handle(&mut self, message: LibraryImportKey, _: &mut Context<Self>) -> Self::Result {
        let root = self.key_root.clone();
        self.mutate(async move {
            let LibraryImportKey {
                platform,
                namespace,
                key,
                title_id,
            } = message;
            if key.is_empty() {
                return Err("TypeError: key must not be empty".into());
            }
            let supplied_id = title_id.as_deref().filter(|id| !id.is_empty());
            if title_id.is_some() && supplied_id.is_none() {
                return Err("TypeError: titleid must be a non-empty string".into());
            }
            if !matches!(namespace.as_str(), "system" | "content") {
                return Err("TypeError: namespace must be system or content".into());
            }
            let id = supplied_id
                .map(str::to_owned)
                .unwrap_or_else(|| blake3::hash(&key).to_hex().to_string());
            let record = KeyRecord {
                identity: KeyIdentity {
                    namespace: namespace.clone(),
                    id,
                },
                title_id: (namespace == "content").then(|| title_id).flatten(),
                bytes: key,
            };
            let description = ImportedKey {
                namespace: record.identity.namespace.clone(),
                id: record.identity.id.clone(),
                title_id: record.title_id.clone(),
                byte_length: record.bytes.len(),
            };
            let store = polymer_library::keys::KeyStore::open(root)
                .await
                .map_err(|error| error.to_string())?;
            store
                .for_platform(&platform)
                .map_err(|error| error.to_string())?
                .put(&[record])
                .await
                .map_err(|error| error.to_string())?;
            Ok(description)
        })
    }
}
