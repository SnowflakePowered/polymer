use super::*;
use polymer_library::{GameCatalog, IdentifiedLaunchable, PackageFile, PackageManifest};
use polymer_pluginhost::loading::PluginRegistry;
use std::{sync::Arc, time::UNIX_EPOCH};

struct Fixture {
    directory: tempfile::TempDir,
    manifest: PackageManifest,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let cue = concat!(
            "REM SINGLE-DENSITY AREA\r\n",
            "CDTEXTFILE \"disc text.dat\"\r\n",
            "FILE \"shared.bin\" BINARY\r\n",
            "  TRACK 01 MODE2/2352\r\n    INDEX 01 00:00:00\r\n",
            "  TRACK 02 AUDIO\r\n    INDEX 00 00:01:00\r\n    INDEX 01 00:03:00\r\n",
            "REM HIGH-DENSITY AREA\r\n",
            "FILE \"last track.bin\" BINARY\r\n",
            "  TRACK 03 MODE1/2352\r\n    INDEX 01 00:00:00\r\n",
        );
        let descriptor = Self::file(directory.path(), "source.cue", cue.as_bytes());
        let files = vec![
            Self::file(directory.path(), "shared.bin", &[1; 2352]),
            Self::file(directory.path(), "last track.bin", &[2; 2352]),
            Self::file(directory.path(), "disc text.dat", b"CD text"),
        ];
        Self {
            directory,
            manifest: PackageManifest {
                version: 1,
                revision: "first".into(),
                descriptor,
                files,
                cue: cue.into(),
                template: None,
                error: None,
            },
        }
    }

    fn file(root: &Path, name: &str, bytes: &[u8]) -> PackageFile {
        let path = root.join(name);
        std::fs::write(&path, bytes).unwrap();
        let path = path.canonicalize().unwrap();
        let stat = std::fs::metadata(&path).unwrap();
        PackageFile {
            name: name.into(),
            path,
            size: stat.len(),
            modified_ns: stat
                .modified()
                .unwrap()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                .to_string(),
        }
    }

    fn input(&self) -> IdentifiedLaunchable {
        IdentifiedLaunchable {
            path: self.manifest.descriptor.path.clone(),
            binary_blake3: [42; 32],
            platform: "SONY_PSX".into(),
            serial: None,
            title_id: None,
            parent_binary_id: None,
        }
    }

    async fn catalog(&self) -> Arc<GameCatalog> {
        Arc::new(
            GameCatalog::open(
                self.directory.path().join("library.db"),
                Arc::new(object_store::memory::InMemory::new()),
            )
            .await
            .unwrap(),
        )
    }

    fn engine(&self, catalog: Arc<GameCatalog>) -> ScrapingEngine {
        ScrapingEngine::new(
            catalog,
            PluginRegistry::new(),
            polymer_library::Directories::rooted(self.directory.path(), self.directory.path()),
        )
    }
}

#[test]
fn template_preserves_layout_and_shared_file_tracks_and_binds_cdtext() {
    let mut fixture = Fixture::new();
    CuePreparation::finalize(&mut fixture.manifest).unwrap();
    let bundle = CuePreparation::bundle(&fixture.manifest).unwrap();
    let rendered = bundle.files.last().unwrap().text.as_ref().unwrap();
    let expected = fixture
        .manifest
        .cue
        .replace("shared.bin", "file-0000.bin")
        .replace("last track.bin", "file-0001.bin")
        .replace("disc text.dat", "file-0002.dat");
    assert_eq!(rendered, &expected);
    let parsed = cuna::CueSheet::new(rendered).unwrap();
    assert_eq!(parsed.files[0].tracks.len(), 2);
    assert_eq!(parsed.files[1].tracks.len(), 1);
    assert_eq!(bundle.files.len(), 4);
    // The generated CUE can be parsed and every reference resolves in its own directory.
    let mounted = tempfile::tempdir().unwrap();
    for file in &bundle.files {
        if let Some(source) = &file.path {
            std::fs::hard_link(source, mounted.path().join(&file.name)).unwrap();
        } else {
            std::fs::write(mounted.path().join(&file.name), file.text.as_ref().unwrap()).unwrap();
        }
    }
    for file in parsed.files {
        assert!(mounted.path().join(file.name).is_file());
    }
    assert!(
        mounted
            .path()
            .join(parsed.header.cdtextfile.unwrap())
            .is_file()
    );
    std::fs::write(&fixture.manifest.files[0].path, b"replaced").unwrap();
    assert!(
        CuePreparation::bundle(&fixture.manifest)
            .unwrap_err()
            .to_string()
            .contains("changed")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn pending_import_recovers_and_stale_finalizer_cannot_overwrite_reimport() {
    let fixture = Fixture::new();
    let catalog = fixture.catalog().await;
    let (launchable, _) = catalog
        .import_package(fixture.input(), vec![], Some(fixture.manifest.clone()))
        .await
        .unwrap();
    let mut package =
        Package::from_launchable(catalog.launchable(launchable.id).await.unwrap()).unwrap();
    assert!(
        package
            .prepare_launch()
            .unwrap_err()
            .to_string()
            .contains("pending")
    );
    // No scrape job was queued before the simulated restart.
    let engine = fixture.engine(catalog.clone());
    engine.resume().await.unwrap();
    let stored = catalog.launchable(launchable.id).await.unwrap();
    let first = stored.manifest.clone().unwrap();
    let mut package = Package::from_launchable(stored).unwrap();
    package.prepare_launch().unwrap();
    assert_eq!(package.bundle.unwrap().entrypoint, "disc.cue");
    let mut next = fixture.manifest.clone();
    next.revision = "second".into();
    catalog
        .import_package(fixture.input(), vec![], Some(next))
        .await
        .unwrap();
    catalog
        .finish_manifest(launchable.id, &first)
        .await
        .unwrap();
    let current = catalog
        .launchable(launchable.id)
        .await
        .unwrap()
        .manifest
        .unwrap();
    assert_eq!(current.revision, "second");
    assert!(current.template.is_none());
    let job = engine
        .enqueue(launchable.binary_id, launchable.binary_id)
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if catalog
                .launchable(launchable.id)
                .await
                .unwrap()
                .manifest
                .unwrap()
                .template
                .is_some()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        catalog.scrape_job(job.id).await.unwrap().unwrap().state,
        polymer_library::ScrapeJobState::Completed
    );
    catalog.remove_launchable(launchable.id).await.unwrap();
    assert!(catalog.game(launchable.binary_id).await.unwrap().is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn missing_member_is_a_durable_preparation_error() {
    let fixture = Fixture::new();
    let catalog = fixture.catalog().await;
    let (launchable, _) = catalog
        .import_package(fixture.input(), vec![], Some(fixture.manifest.clone()))
        .await
        .unwrap();
    std::fs::remove_file(&fixture.manifest.files[1].path).unwrap();
    fixture.engine(catalog.clone()).resume().await.unwrap();
    let stored = catalog.launchable(launchable.id).await.unwrap();
    assert!(
        stored
            .manifest
            .as_ref()
            .unwrap()
            .error
            .as_ref()
            .unwrap()
            .contains("last track.bin")
    );
    assert!(
        Package::from_launchable(stored)
            .unwrap()
            .prepare_launch()
            .is_err()
    );
}
