use object_store::memory::InMemory;
use polymer_library::{
    GameCatalog, IdentifiedLaunchable, MetadataData, MetadataInput, MetadataInputData,
    MetadataValue, ScrapeJobState,
};
use polymer_pluginhost::loading::PluginRegistry;
use polymer_scraping::{ScrapingEngine, ScrapingEvent};
use std::{sync::Arc, time::Duration};

#[tokio::test(flavor = "current_thread")]
async fn successful_plugins_commit_repeated_values_and_publish_events() {
    let directory = tempfile::tempdir().unwrap();
    let plugin = tempfile::tempdir().unwrap();
    std::fs::write(
        plugin.path().join("plugin.toml"),
        r#"
          [plugin]
          id = "fixture"
          version = "1"
          class = "scraper"
          main = "fixture.js"

          [scraper]
          requires = ["package.platform", "any(package.serial, package.files)"]
          provides = ["meta.genre"]
        "#,
    )
    .unwrap();
    std::fs::write(
        plugin.path().join("fixture.js"),
        r#"
          export async function* onScrape(input) {
            const { packages } = input;
            await new Promise(resolve => setTimeout(resolve, 10));
            if (input["package.platform"][0].value !== "NINTENDO_NES") return;
            if (packages.length !== 1 || packages[0].files.length !== 1) throw Error("missing raw file input");
            const pkg = packages[0], file = pkg.files[0];
            if ("binaryDigest" in pkg || input["package.hash.sha1"]) throw Error("eager hash leaked");
            if (await file.digest("blake3") !== await pkg.digest("blake3")) throw Error("wrong package bytes");
            if (await file.digest("blake3") === await pkg.binary.digest("blake3")) throw Error("binary was not normalized");
            yield { key: "meta.genre", value: "Action", source: "Fixture DB" };
            yield { key: "meta.genre", value: "Adventure", source: "Fixture DB" };
          }
        "#,
    )
    .unwrap();
    let registry = PluginRegistry::new();
    registry.load(plugin.path()).unwrap();
    let catalog = Arc::new(
        GameCatalog::open(
            directory.path().join("library.db"),
            Arc::new(InMemory::new()),
        )
        .await
        .unwrap(),
    );
    let mut image = vec![0; 16];
    image[..4].copy_from_slice(b"NES\x1a");
    image.extend_from_slice(b"engine-fixture");
    std::fs::write(directory.path().join("game.nes"), image).unwrap();
    let (launchable, _) = catalog
        .import_launchable_seeded(
            IdentifiedLaunchable {
                path: directory.path().join("game.nes"),
                binary_blake3: *blake3::hash(b"engine-fixture").as_bytes(),
                serial: None,
                title_id: None,
                parent_binary_id: None,
                platform: "NINTENDO_NES".to_owned(),
            },
            vec![(
                "host:gumshoe".to_owned(),
                vec![MetadataInput {
                    key: "package.platform".to_owned(),
                    source: "Gumshoe".to_owned(),
                    data: MetadataInputData::Scalar {
                        value: MetadataValue::String("NINTENDO_NES".to_owned()),
                    },
                }],
            )],
        )
        .await
        .unwrap();
    let engine = ScrapingEngine::new(
        catalog.clone(),
        registry,
        polymer_library::Directories::rooted(directory.path(), directory.path()),
    );
    let mut events = engine.subscribe();
    let job = engine
        .enqueue(launchable.binary_id, launchable.binary_id)
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let current = catalog.scrape_job(job.id).await.unwrap().unwrap();
            if current.state == ScrapeJobState::Completed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("scrape job did not complete");

    let values = catalog
        .metadata(launchable.binary_id)
        .await
        .unwrap()
        .into_iter()
        .filter(|value| value.producer == "scraper:fixture")
        .collect::<Vec<_>>();
    assert_eq!(values.len(), 2);
    assert_eq!(values[0].ordinal, 0);
    assert_eq!(values[1].ordinal, 1);
    assert_eq!(
        values[0].data,
        MetadataData::Scalar(MetadataValue::String("Action".to_owned()))
    );
    assert_eq!(
        values[1].data,
        MetadataData::Scalar(MetadataValue::String("Adventure".to_owned()))
    );

    let mut saw_content = false;
    let mut saw_running_attempt = false;
    while let Ok(event) = events.try_recv() {
        match event {
            ScrapingEvent::LibraryContentChanged { .. } => saw_content = true,
            ScrapingEvent::JobChanged(job) => {
                saw_running_attempt |= job
                    .attempts
                    .iter()
                    .any(|attempt| attempt.state == polymer_library::ScrapeAttemptState::Running);
            }
        }
    }
    assert!(saw_content);
    assert!(
        saw_running_attempt,
        "themes must be notified when a scraper starts"
    );
}
