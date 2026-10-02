use object_store::memory::InMemory;
use polymer_library::{
    GameCatalog, IdentifiedLaunchable, LaunchableUri, MetadataData, MetadataInput,
    MetadataInputData, MetadataValue,
    jobs::{JobKind, JobSpec, JobState, JobStore, JobSubject},
};
use polymer_pluginhost::loading::PluginRegistry;
use polymer_scraping::{Ingestor, Package, ScrapingEngine, ScrapingEvent};
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
        .import_package(
            IdentifiedLaunchable {
                manifest: None,
                path: LaunchableUri::file(directory.path().join("game.nes")).unwrap(),
                entrypoint: None,
                package: None,
                binary_blake3: *blake3::hash(b"engine-fixture").as_bytes(),
                serial: None,
                title_id: None,
                disc_number: None,
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
    let jobs = JobStore::open(directory.path().join("jobs.db"))
        .await
        .unwrap();
    let engine = ScrapingEngine::new(
        catalog.clone(),
        jobs.clone(),
        registry,
        polymer_library::Directories::rooted(directory.path(), directory.path()),
    );
    let mut events = engine.subscribe();
    let mut changes = jobs.subscribe();
    let job = engine
        .enqueue(launchable.binary_id, launchable.binary_id)
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let current = jobs.get(job.id).await.unwrap().unwrap();
            if current.state == JobState::Completed {
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
    while let Ok(ScrapingEvent::LibraryContentChanged { .. }) = events.try_recv() {
        saw_content = true;
    }
    let mut saw_running_attempt = false;
    while let Ok(job) = changes.try_recv() {
        saw_running_attempt |= job.steps.iter().any(|step| step.state == JobState::Running);
    }
    assert!(saw_content);
    assert!(
        saw_running_attempt,
        "themes must be notified when a scraper starts"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn child_packages_keep_their_own_identity_and_scrape_results() {
    let directory = tempfile::tempdir().unwrap();
    let plugin = tempfile::tempdir().unwrap();
    std::fs::write(
        plugin.path().join("plugin.toml"),
        r#"
          [plugin]
          id = "identity"
          version = "1"
          class = "scraper"
          main = "identity.js"

          [scraper]
          requires = ["package"]
          provides = ["meta.description"]
        "#,
    )
    .unwrap();
    std::fs::write(
        plugin.path().join("identity.js"),
        r#"
          export async function* onScrape(input) {
            for (const package_ of input.packages) {
              yield {
                key: "meta.description",
                value: await package_.binary.digest("blake3"),
                source: "Package identity",
              };
            }
          }
        "#,
    )
    .unwrap();
    let catalog = Arc::new(
        GameCatalog::open(
            directory.path().join("library.db"),
            Arc::new(InMemory::new()),
        )
        .await
        .unwrap(),
    );
    let mut launchables = Vec::new();
    let mut parent = None;
    for (name, contents) in [("root.nes", 1), ("child.nes", 2)] {
        let path = directory.path().join(name);
        let mut image = vec![0; 16];
        image[..4].copy_from_slice(b"NES\x1a");
        image[4] = 1;
        image.extend(vec![contents; 16 * 1024]);
        std::fs::write(&path, image).unwrap();
        let package = Package::identify(&path, None, parent).unwrap();
        let (launchable, created) = package.import(&catalog).await.unwrap();
        assert!(created, "each newly identified binary needs a scrape");
        parent = Some(
            launchable
                .identified
                .parent_binary_id
                .unwrap_or(launchable.binary_id),
        );
        launchables.push(launchable);
    }
    let root = launchables[0].binary_id;
    assert_ne!(root, launchables[1].binary_id);
    assert_eq!(launchables[1].identified.parent_binary_id, Some(root));

    // Both requests are pending before workers start: a child must not be
    // coalesced into the root's job or overwrite its eventual metadata.
    let store = JobStore::open(directory.path().join("jobs.db"))
        .await
        .unwrap();
    let mut jobs = Vec::new();
    for launchable in &launchables {
        let (root, trigger) = (
            root.simple().to_string(),
            launchable.binary_id.simple().to_string(),
        );
        let (job, _) = store
            .enqueue(JobSpec {
                kind: JobKind::Scrape,
                parent: None,
                title: "Scraping metadata".to_owned(),
                subject: JobSubject {
                    binary_id: Some(root.clone()),
                    ..Default::default()
                },
                params: serde_json::json!({ "rootBinaryId": root, "triggerBinaryId": trigger }),
                dedupe: Some(format!("{root}:{trigger}")),
                steps: vec!["identity".to_owned()],
                persistent: true,
            })
            .await
            .unwrap();
        jobs.push(job);
    }
    let registry = PluginRegistry::new();
    registry.load(plugin.path()).unwrap();
    let engine = ScrapingEngine::new(
        catalog.clone(),
        store.clone(),
        registry,
        polymer_library::Directories::rooted(directory.path(), directory.path()),
    );
    engine.resume().await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        for job in &jobs {
            loop {
                let current = store.get(job.id).await.unwrap().unwrap();
                match current.state {
                    JobState::Completed => break,
                    JobState::Queued | JobState::Running => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    _ => panic!("scrape failed: {current:?}"),
                }
            }
        }
    })
    .await
    .expect("root and child scrapes did not complete");

    for launchable in &launchables {
        let descriptions = catalog
            .metadata(launchable.binary_id)
            .await
            .unwrap()
            .into_iter()
            .filter(|value| value.producer == "scraper:identity")
            .collect::<Vec<_>>();
        assert_eq!(descriptions.len(), 1);
        let digest = blake3::Hash::from_bytes(launchable.identified.binary_blake3);
        assert_eq!(
            descriptions[0].data,
            MetadataData::Scalar(MetadataValue::String(digest.to_hex().to_string()))
        );
        let package = Package::identify(
            launchable.identified.path.host_path().unwrap(),
            None,
            launchable.identified.parent_binary_id,
        )
        .unwrap();
        let imported = Ingestor::import(&engine, vec![package]).await.unwrap();
        assert!(imported[0].1.is_none());
    }
    let game = catalog.game(root).await.unwrap().unwrap();
    assert_eq!(game.launchables.len(), 2);
    assert_eq!(
        game.metadata
            .iter()
            .filter(|value| value.producer == "scraper:identity")
            .count(),
        2
    );
    let scrapes = store
        .list()
        .await
        .unwrap()
        .into_iter()
        .filter(|job| job.kind == JobKind::Scrape)
        .count();
    assert_eq!(scrapes, 2);
}

#[tokio::test(flavor = "current_thread")]
async fn overlapping_refreshes_share_one_job_that_parents_the_plugins_jobs() {
    let directory = tempfile::tempdir().unwrap();
    let plugin = tempfile::tempdir().unwrap();
    std::fs::write(
        plugin.path().join("plugin.toml"),
        r#"
          [plugin]
          id = "contents"
          version = "1"
          class = "scraper"
          main = "contents.js"

          [scraper]
          requires = ["package.platform"]
          provides = ["meta.genre"]
        "#,
    )
    .unwrap();
    std::fs::write(
        plugin.path().join("contents.js"),
        r#"
          import { Job } from "polymer:jobs";
          export async function updateContents() {
            await new Job("Indexing").run(() => new Promise(resolve => setTimeout(resolve, 50)));
          }
          export async function* onScrape() {}
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
    let jobs = JobStore::open(directory.path().join("jobs.db"))
        .await
        .unwrap();
    let engine = ScrapingEngine::new(
        catalog,
        jobs.clone(),
        registry,
        polymer_library::Directories::rooted(directory.path(), directory.path()),
    );
    let first = engine.refresh("contents").await.unwrap();
    let second = engine.refresh("contents").await.unwrap();
    assert_eq!(first.id, second.id);
    let refreshed = tokio::time::timeout(Duration::from_secs(10), jobs.wait(first.id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(refreshed.state, JobState::Completed);
    let listed = jobs.list().await.unwrap();
    let child = listed.iter().find(|job| job.title == "Indexing").unwrap();
    assert_eq!(child.parent, Some(first.id));
    assert_eq!(child.state, JobState::Completed);
}

#[tokio::test(flavor = "current_thread")]
async fn scraping_refreshes_contents_a_plugin_reports_missing_before_its_first_scrape() {
    let directory = tempfile::tempdir().unwrap();
    let plugin = tempfile::tempdir().unwrap();
    std::fs::write(
        plugin.path().join("plugin.toml"),
        r#"
          [plugin]
          id = "contents"
          version = "1"
          class = "scraper"
          main = "contents.js"

          [scraper]
          requires = ["package.platform"]
          provides = ["meta.genre"]
        "#,
    )
    .unwrap();
    std::fs::write(
        plugin.path().join("contents.js"),
        r#"
          import { connect } from "polymer:database";
          async function genre() {
            const db = await connect();
            try { return (await db.prepare("SELECT value FROM genres").get())?.value; }
            catch { return undefined; }
            finally { await db.close(); }
          }
          export async function onPluginLoad() {
            return (await genre()) !== undefined;
          }
          export async function updateContents() {
            const db = await connect();
            try { await db.exec("CREATE TABLE genres (value TEXT); INSERT INTO genres VALUES ('Action');"); }
            finally { await db.close(); }
          }
          export async function* onScrape() {
            const value = await genre();
            if (value === undefined) throw new Error("contents are missing");
            yield { key: "meta.genre", value, source: "Contents" };
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
    std::fs::write(directory.path().join("game.nes"), b"contents-fixture").unwrap();
    let (launchable, _) = catalog
        .import_package(
            IdentifiedLaunchable {
                manifest: None,
                path: LaunchableUri::file(directory.path().join("game.nes")).unwrap(),
                entrypoint: None,
                package: None,
                binary_blake3: *blake3::hash(b"contents-fixture").as_bytes(),
                serial: None,
                title_id: None,
                disc_number: None,
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
    let jobs = JobStore::open(directory.path().join("jobs.db"))
        .await
        .unwrap();
    let engine = ScrapingEngine::new(
        catalog.clone(),
        jobs.clone(),
        registry,
        polymer_library::Directories::rooted(directory.path(), directory.path()),
    );
    let job = engine
        .enqueue(launchable.binary_id, launchable.binary_id)
        .await
        .unwrap();
    let scraped = tokio::time::timeout(Duration::from_secs(10), jobs.wait(job.id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(scraped.state, JobState::Completed, "{:?}", scraped.steps);
    let genres = catalog
        .metadata(launchable.binary_id)
        .await
        .unwrap()
        .into_iter()
        .filter(|value| value.producer == "scraper:contents")
        .map(|value| value.data)
        .collect::<Vec<_>>();
    assert_eq!(
        genres,
        [MetadataData::Scalar(MetadataValue::String(
            "Action".to_owned()
        ))]
    );
    let refreshes = jobs
        .list()
        .await
        .unwrap()
        .into_iter()
        .filter(|job| job.kind == JobKind::Refresh)
        .collect::<Vec<_>>();
    assert_eq!(refreshes.len(), 1);
    assert_eq!(refreshes[0].state, JobState::Completed);
}

#[tokio::test(flavor = "current_thread")]
async fn a_titles_cached_updates_are_replaced_by_its_latest_check() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = GameCatalog::open(
        directory.path().join("library.db"),
        Arc::new(InMemory::new()),
    )
    .await
    .unwrap();
    std::fs::write(directory.path().join("game.iso"), b"updates-fixture").unwrap();
    let (launchable, _) = catalog
        .import_package(
            IdentifiedLaunchable {
                manifest: None,
                path: LaunchableUri::file(directory.path().join("game.iso")).unwrap(),
                entrypoint: None,
                package: None,
                binary_blake3: *blake3::hash(b"updates-fixture").as_bytes(),
                serial: None,
                title_id: Some("BLES00229".to_owned()),
                disc_number: None,
                parent_binary_id: None,
                platform: "SONY_PS3".to_owned(),
            },
            Vec::new(),
        )
        .await
        .unwrap();
    let candidate =
        |version: &str| serde_json::json!({ "titleId": "BLES00229", "version": version });
    catalog
        .replace_updates(launchable.id, &[candidate("01.01"), candidate("01.02")], 1)
        .await
        .unwrap();
    catalog
        .replace_updates(launchable.id, &[candidate("01.02")], 2)
        .await
        .unwrap();
    let game = catalog.game(launchable.binary_id).await.unwrap().unwrap();
    assert_eq!(game.updates[&launchable.id], [candidate("01.02")]);
}
