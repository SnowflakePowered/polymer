use super::LibraryBackend;
use crate::library::LicenseReader;
use actix::{AtomicResponse, Context, Handler, ResponseActFuture, WrapFuture};
use polymer_consolestorage::impls::ctr::{CtrNand, seeddb::CtrSeedDb};
use polymer_consolestorage::impls::ps3::{Ps3Nand, depots::Ps3PupDepot, rap::Rap};
use polymer_consolestorage::installable::Depot;
use polymer_consolestorage::keys::{KeyIdentity, KeyQuery, KeyReader, KeyRecord, KeyWriter};
use polymer_deno::polymer::library::{
    ImportedKey, KeysLicensed, KeysList, KeysPlatforms, KeysRemove, LibraryError, LibraryImport,
    LibraryImportKey,
};
use polymer_scraping::Ingestor;
use serde_json::{Value, json};
use std::path::PathBuf;

impl Handler<LibraryImport> for LibraryBackend {
    type Result = AtomicResponse<Self, Result<Value, LibraryError>>;

    fn handle(
        &mut self,
        LibraryImport { path }: LibraryImport,
        _: &mut Context<Self>,
    ) -> Self::Result {
        let scraping = self.scraping.clone();
        let firmware_root = self.firmware_root.clone();
        let key_root = self.key_root.clone();
        let locks = self.locks.clone();
        self.mutate(async move {
            let path = PathBuf::from(path);
            let _guard = locks.lock(&path).await;
            if path.is_file() && path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("rap")) {
                let record = Rap::read(&path).map_err(|error| error.to_string())?;
                let store = polymer_library::keys::KeyStore::open(key_root).await.map_err(|error| error.to_string())?;
                store.for_platform(Ps3Nand::PLATFORM).map_err(|error| error.to_string())?
                    .put(&[record]).await.map_err(|error| error.to_string())?;
                return Ok(json!({ "kind": "keys", "platform": Ps3Nand::PLATFORM, "count": 1 }));
            }
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
                Ok(packages) => Ok(json!({ "kind": "games", "imports": Self::import_games(&scraping, packages, polymer_library::ImportOrigin::Manual).await? })),
                Err(error) if error.firmware_candidate() && path.is_file() => {
                    let probe = path.clone();
                    let pup = tokio::task::spawn_blocking(move || Ps3PupDepot::open(&probe))
                        .await.map_err(|error| format!("system software detection task failed: {error}"))?;
                    if let Ok(pup) = pup {
                        return Ok(json!({
                            "kind": "system",
                            "platform": Ps3Nand::PLATFORM,
                            "path": path,
                            "filename": path.file_name().map(|name| name.to_string_lossy()),
                            "version": pup.version(),
                        }));
                    }
                    let seeds = match std::fs::metadata(&path) {
                        Ok(metadata) if metadata.len() % 32 == 16 => tokio::fs::read(&path).await
                            .ok().and_then(|bytes| CtrSeedDb::decode(&bytes).ok()),
                        _ => None,
                    };
                    if let Some(seeds) = seeds {
                        let store = polymer_library::keys::KeyStore::open(key_root).await.map_err(|error| error.to_string())?;
                        for platform in [CtrNand::PLATFORM, CtrNand::NEW_PLATFORM] {
                            store.for_platform(platform).map_err(|error| error.to_string())?
                                .put(&seeds).await.map_err(|error| error.to_string())?;
                        }
                        return Ok(json!({ "kind": "keys", "platform": CtrNand::PLATFORM, "count": seeds.len() }));
                    }
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
    type Result = AtomicResponse<Self, Result<ImportedKey, LibraryError>>;

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
                return Err(LibraryError::Type("key must not be empty".into()));
            }
            let supplied_id = title_id.as_deref().filter(|id| !id.is_empty());
            if title_id.is_some() && supplied_id.is_none() {
                return Err(LibraryError::Type(
                    "titleid must be a non-empty string".into(),
                ));
            }
            if !matches!(namespace.as_str(), "system" | "content") {
                return Err(LibraryError::Type(
                    "namespace must be system or content".into(),
                ));
            }
            let id = supplied_id
                .map(str::to_owned)
                .unwrap_or_else(|| blake3::hash(&key).to_hex().to_string());
            let record = KeyRecord {
                identity: KeyIdentity {
                    namespace: namespace.clone(),
                    id,
                },
                title_id: (namespace == "content").then_some(title_id).flatten(),
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

impl Handler<KeysList> for LibraryBackend {
    type Result = AtomicResponse<Self, Result<Value, LibraryError>>;

    fn handle(
        &mut self,
        KeysList {
            platform,
            namespace,
        }: KeysList,
        _: &mut Context<Self>,
    ) -> Self::Result {
        let root = self.key_root.clone();
        self.mutate(async move {
            let store = polymer_library::keys::KeyStore::open(root)
                .await
                .map_err(|error| error.to_string())?;
            let keys = store
                .for_platform(&platform)
                .map_err(|error| error.to_string())?
                .list(&KeyQuery {
                    namespace,
                    title_id: None,
                })
                .await
                .map_err(|error| error.to_string())?;
            serde_json::to_value(keys).map_err(|error| error.to_string())
        })
    }
}

impl Handler<KeysPlatforms> for LibraryBackend {
    type Result = AtomicResponse<Self, Result<Vec<String>, LibraryError>>;

    fn handle(&mut self, _: KeysPlatforms, _: &mut Context<Self>) -> Self::Result {
        let root = self.key_root.clone();
        self.mutate(async move {
            polymer_library::keys::KeyStore::open(root)
                .await
                .map_err(|error| error.to_string())?
                .platforms()
                .await
                .map_err(|error| error.to_string())
        })
    }
}

impl Handler<KeysLicensed> for LicenseReader {
    type Result = ResponseActFuture<Self, Result<bool, LibraryError>>;

    fn handle(
        &mut self,
        KeysLicensed { platform, title_id }: KeysLicensed,
        _: &mut Context<Self>,
    ) -> Self::Result {
        let keys = self.keys.clone();
        let catalog = self.catalog.clone();
        Box::pin(
            async move {
                if !catalog
                    .requires_license(&platform, &title_id)
                    .await
                    .map_err(|error| error.to_string())?
                {
                    return Ok(true);
                }
                keys.for_platform(&platform)
                    .map_err(|error| error.to_string())?
                    .licensed(&title_id)
                    .await
                    .map_err(|error| LibraryError::Generic(error.to_string()))
            }
            .into_actor(self),
        )
    }
}

impl Handler<KeysRemove> for LibraryBackend {
    type Result = AtomicResponse<Self, Result<bool, LibraryError>>;

    fn handle(
        &mut self,
        KeysRemove {
            platform,
            namespace,
            id,
        }: KeysRemove,
        _: &mut Context<Self>,
    ) -> Self::Result {
        let root = self.key_root.clone();
        self.mutate(async move {
            polymer_library::keys::KeyStore::open(root)
                .await
                .map_err(|error| error.to_string())?
                .for_platform(&platform)
                .map_err(|error| error.to_string())?
                .remove(&KeyIdentity { namespace, id })
                .await
                .map_err(|error| error.to_string())
        })
    }
}
