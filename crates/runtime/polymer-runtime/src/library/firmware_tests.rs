use super::LibraryService;
use polymer_deno::polymer::library::{GamesList, LaunchablesAdd, LibraryImport, ScrapingJobs};
use polymer_library::{Directories, firmware::FirmwareStore};
use polymer_pluginhost::loading::PluginRegistry;

struct Fixture {
    root: tempfile::TempDir,
    directories: Directories,
    registry: PluginRegistry,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let plugin = root.path().join("plugin");
        std::fs::create_dir(&plugin).unwrap();
        std::fs::write(
            plugin.join("plugin.toml"),
            r#"
            [plugin]
            id = "firmware-fixture"
            version = "1"
            class = "platform"
            main = "main.js"
            [platform]
            platforms = ["SONY_PSX"]
        "#,
        )
        .unwrap();
        std::fs::write(
            plugin.join("main.js"),
            r#"
            export async function onCheckFirmware(file) {
                if (file.name === 'unrecognized.bin') return;
                if (await file.text() !== 'hardware') throw Error('RECOGNIZER_SHOULD_NOT_RUN');
                return [{ platform: 'SONY_PSX', type: 'bios', label: 'Fixture BIOS' }];
            }
        "#,
        )
        .unwrap();
        let registry = PluginRegistry::new();
        registry.load(plugin).unwrap();
        Self {
            directories: Directories::rooted(root.path(), root.path()),
            root,
            registry,
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn firmware_import_does_not_create_games_or_scrape_jobs() {
    let fixture = Fixture::new();
    let service = LibraryService::open_with_plugins(&fixture.directories, fixture.registry.clone())
        .await
        .unwrap();
    let client = service.client();
    let path = fixture.root.path().join("hardware.bin");
    std::fs::write(&path, b"hardware").unwrap();
    // The game-only API retains its rejection and does not install firmware.
    assert!(
        client
            .launchables_add
            .send(LaunchablesAdd {
                path: path.to_string_lossy().into_owned(),
                platform: None,
                parent_binary_id: None
            })
            .await
            .unwrap()
            .is_err()
    );
    let result = client
        .import
        .send(LibraryImport {
            path: path.to_string_lossy().into_owned(),
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result["kind"], "firmware");
    assert_eq!(result["firmware"][0]["label"], "Fixture BIOS");
    assert!(
        client
            .games_list
            .send(GamesList { platform: None })
            .await
            .unwrap()
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        client
            .scraping_jobs
            .send(ScrapingJobs)
            .await
            .unwrap()
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    let store = FirmwareStore::open(fixture.directories.firmware())
        .await
        .unwrap();
    let records = store.list("SONY_PSX").await.unwrap();
    let (_, stored) = store
        .file("SONY_PSX", &records[0].firmware_type, &records[0].label)
        .await
        .unwrap();
    assert_eq!(std::fs::read(stored).unwrap(), b"hardware");
}

#[tokio::test(flavor = "multi_thread")]
async fn game_import_and_access_errors_do_not_run_firmware_recognition() {
    let fixture = Fixture::new();
    let service = LibraryService::open_with_plugins(&fixture.directories, fixture.registry.clone())
        .await
        .unwrap();
    let client = service.client();
    let path = fixture.root.path().join("game.nes");
    let mut image = vec![0; 16];
    image[..4].copy_from_slice(b"NES\x1a");
    image.extend_from_slice(&[0x42; 32]);
    std::fs::write(&path, image).unwrap();
    let result = client
        .import
        .send(LibraryImport {
            path: path.to_string_lossy().into_owned(),
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result["kind"], "games");
    assert_eq!(result["imports"].as_array().unwrap().len(), 1);
    for filename in ["missing.bin", "unrecognized.bin"] {
        let path = fixture.root.path().join(filename);
        if filename == "unrecognized.bin" {
            std::fs::write(&path, b"unknown").unwrap();
        }
        let error = client
            .import
            .send(LibraryImport {
                path: path.to_string_lossy().into_owned(),
            })
            .await
            .unwrap()
            .unwrap_err();
        assert!(!error.contains("RECOGNIZER_SHOULD_NOT_RUN"), "{error}");
        if filename == "unrecognized.bin" {
            assert!(error.contains("no platform recognized"));
        }
    }
    let store = FirmwareStore::open(fixture.directories.firmware())
        .await
        .unwrap();
    assert!(store.list("SONY_PSX").await.unwrap().is_empty());
}
