use object_store::memory::InMemory;
use snowglass_library::{
    GameCatalog, IdentifiedLaunchable, MetadataData, MetadataInput, MetadataInputData,
    MetadataValue, ScrapeJobState,
};
use snowglass_pluginhost::PluginRegistry;
use snowglass_scraping::{ScrapingEngine, ScrapingEvent};
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
          requires = ["package.platform"]
          provides = ["meta.genre"]
        "#,
    )
    .unwrap();
    std::fs::write(
        plugin.path().join("fixture.js"),
        r#"
          export async function* onScrape(input) {
            if (input["package.platform"][0].value !== "NINTENDO_NES") return;
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
    let (launchable, _) = catalog
        .import_launchable_seeded(
            IdentifiedLaunchable {
                path: directory.path().join("game.rom"),
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
        snowglass_library::Directories::rooted(directory.path(), directory.path()),
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
    while let Ok(event) = events.try_recv() {
        saw_content |= matches!(event, ScrapingEvent::LibraryContentChanged { .. });
    }
    assert!(saw_content);
}
