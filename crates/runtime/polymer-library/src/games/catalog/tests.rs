use super::*;
use object_store::{ObjectStoreExt, memory::InMemory};

#[tokio::test]
async fn launchable_configuration_migrates_isolates_and_cascades() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("library.db");
    let objects = Arc::new(InMemory::new());
    let store = GameCatalog::open(&path, objects.clone()).await.unwrap();
    let mut ids = Vec::new();
    for name in ["one.rom", "two.rom"] {
        ids.push(
            store
                .import_launchable(IdentifiedLaunchable {
                    path: directory.path().join(name),
                    binary_blake3: [7; 32],
                    serial: Some("SERIAL".into()),
                    title_id: Some("TITLE".into()),
                    parent_binary_id: None,
                    platform: "NINTENDO_NES".into(),
                })
                .await
                .unwrap()
                .id,
        );
    }
    store
        .connection()
        .await
        .unwrap()
        .execute_batch("DROP TABLE launchable_configurations; PRAGMA user_version = 2;")
        .await
        .unwrap();
    drop(store);
    let store = GameCatalog::open(&path, objects.clone()).await.unwrap();
    assert_eq!(
        store.launchable(ids[0]).await.unwrap().serial.as_deref(),
        Some("SERIAL")
    );
    let values = serde_json::json!({"graphics": {"enabled": true, "scale": 2, "mode": "vulkan", "optional": null}});
    store
        .save_configuration(ids[0], "emulator", "v1", &values)
        .await
        .unwrap();
    for (id, plugin, schema) in [
        (ids[1], "emulator", "v1"),
        (ids[0], "other", "v1"),
        (ids[0], "emulator", "v2"),
    ] {
        assert!(
            store
                .configuration(id, plugin, schema)
                .await
                .unwrap()
                .is_none()
        );
    }
    assert!(
        store
            .save_configuration(
                ids[0],
                "emulator",
                "v1",
                &serde_json::json!({"graphics": {"bad": []}})
            )
            .await
            .is_err()
    );
    drop(store);
    let store = GameCatalog::open(&path, objects).await.unwrap();
    assert_eq!(
        store.configuration(ids[0], "emulator", "v1").await.unwrap(),
        Some(values.clone())
    );
    store
        .save_configuration(ids[0], "emulator", "v2", &serde_json::json!({}))
        .await
        .unwrap();
    assert_eq!(
        store.configuration(ids[0], "emulator", "v1").await.unwrap(),
        Some(values)
    );
    store.remove_launchable(ids[0]).await.unwrap();
    let connection = store.connection().await.unwrap();
    let mut rows = connection
        .query("SELECT COUNT(*) FROM launchable_configurations", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
    assert!(store.configuration(ids[0], "emulator", "v1").await.is_err());
    assert!(
        store
            .save_configuration(ids[0], "emulator", "v1", &serde_json::json!({}))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn catalog_deduplicates_binaries_and_owns_external_media() {
    let directory = tempfile::tempdir().unwrap();
    let objects = Arc::new(InMemory::new());
    let store = GameCatalog::open(directory.path().join("library.db"), objects.clone())
        .await
        .unwrap();
    let digest = *blake3::hash(b"normalized game binary").as_bytes();
    let first = store
        .import_launchable(IdentifiedLaunchable {
            path: directory.path().join("one.rom"),
            binary_blake3: digest,
            serial: Some("SERIAL".to_owned()),
            title_id: None,
            parent_binary_id: None,
            platform: "NINTENDO_NES".to_owned(),
        })
        .await
        .unwrap();
    let duplicate = store
        .import_launchable(IdentifiedLaunchable {
            path: directory.path().join("two.rom"),
            binary_blake3: digest,
            serial: Some("SERIAL".to_owned()),
            title_id: None,
            parent_binary_id: None,
            platform: "NINTENDO_NES".to_owned(),
        })
        .await
        .unwrap();
    let refreshed = store
        .import_launchable(IdentifiedLaunchable {
            path: directory.path().join("one.rom"),
            binary_blake3: digest,
            serial: Some("SERIAL".to_owned()),
            title_id: None,
            parent_binary_id: None,
            platform: "NINTENDO_NES".to_owned(),
        })
        .await
        .unwrap();

    assert_eq!(first.binary_id, duplicate.binary_id);
    assert_eq!(first.id, refreshed.id);
    assert_eq!(store.games(None).await.unwrap()[0].launchables.len(), 2);

    let source = directory.path().join("cover.png");
    tokio::fs::write(&source, b"original cover").await.unwrap();
    let metadata = store
        .add_metadata(
            first.binary_id,
            MetadataInput {
                key: "media.boxart.front.en-US".to_owned(),
                source: "test".to_owned(),
                data: MetadataInputData::Object {
                    media_type: "image/png".to_owned(),
                    path: source.clone(),
                    source_url: None,
                },
            },
        )
        .await
        .unwrap();
    tokio::fs::write(source, b"changed source").await.unwrap();
    let MetadataData::Media {
        data: MediaData::Object(key),
        ..
    } = metadata.data
    else {
        panic!("expected object metadata")
    };
    assert_eq!(
        objects.get(&key).await.unwrap().bytes().await.unwrap(),
        &b"original cover"[..]
    );

    store.remove_metadata(metadata.id).await.unwrap();
    assert!(matches!(
        objects.head(&key).await,
        Err(object_store::Error::NotFound { .. })
    ));
}

#[tokio::test]
async fn deleting_the_final_standalone_launchable_cascades_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let store = GameCatalog::open(
        directory.path().join("library.db"),
        Arc::new(InMemory::new()),
    )
    .await
    .unwrap();
    let launchable = store
        .import_launchable(IdentifiedLaunchable {
            path: directory.path().join("game.rom"),
            binary_blake3: *blake3::hash(b"game").as_bytes(),
            serial: None,
            title_id: None,
            parent_binary_id: None,
            platform: "NINTENDO_NES".to_owned(),
        })
        .await
        .unwrap();
    let year = store
        .add_metadata(
            launchable.binary_id,
            MetadataInput {
                key: "meta.releaseyear".to_owned(),
                source: "test".to_owned(),
                data: MetadataInputData::Scalar {
                    value: MetadataValue::Number(1987.0),
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(store.metadata_text(year.id).await.unwrap(), "1987");

    store.remove_launchable(launchable.id).await.unwrap();
    assert!(store.game(launchable.binary_id).await.unwrap().is_none());
    assert!(matches!(
        store.metadata_text(year.id).await,
        Err(GameCatalogError::NotFound(_))
    ));
}

#[tokio::test]
async fn producer_replacement_preserves_other_sources_and_repeated_ordinals() {
    let directory = tempfile::tempdir().unwrap();
    let store = GameCatalog::open(
        directory.path().join("library.db"),
        Arc::new(InMemory::new()),
    )
    .await
    .unwrap();
    let launchable = store
        .import_launchable(IdentifiedLaunchable {
            path: directory.path().join("game.rom"),
            binary_blake3: *blake3::hash(b"producer-test").as_bytes(),
            serial: None,
            title_id: None,
            parent_binary_id: None,
            platform: "NINTENDO_NES".to_owned(),
        })
        .await
        .unwrap();
    let scalar = |value: &str| MetadataInput {
        key: "meta.genre".to_owned(),
        source: "Shared attribution".to_owned(),
        data: MetadataInputData::Scalar {
            value: MetadataValue::String(value.to_owned()),
        },
    };

    store
        .replace_metadata(
            launchable.binary_id,
            "scraper:first",
            vec![scalar("Action"), scalar("Adventure")],
        )
        .await
        .unwrap();
    store
        .replace_metadata(
            launchable.binary_id,
            "scraper:second",
            vec![scalar("Platformer")],
        )
        .await
        .unwrap();
    store
        .replace_metadata(
            launchable.binary_id,
            "scraper:first",
            vec![scalar("Metroidvania")],
        )
        .await
        .unwrap();

    let values = store.metadata(launchable.binary_id).await.unwrap();
    assert_eq!(values.len(), 2);
    assert!(values.iter().any(|value| {
        value.producer == "scraper:first"
            && value.ordinal == 0
            && value.data == MetadataData::Scalar(MetadataValue::String("Metroidvania".to_owned()))
    }));
    assert!(values.iter().any(|value| {
        value.producer == "scraper:second"
            && value.data == MetadataData::Scalar(MetadataValue::String("Platformer".to_owned()))
    }));
}

#[tokio::test]
async fn metadata_rejects_non_finite_numbers() {
    let directory = tempfile::tempdir().unwrap();
    let store = GameCatalog::open(
        directory.path().join("library.db"),
        Arc::new(InMemory::new()),
    )
    .await
    .unwrap();
    let launchable = store
        .import_launchable(IdentifiedLaunchable {
            path: directory.path().join("game.rom"),
            binary_blake3: *blake3::hash(b"finite-test").as_bytes(),
            serial: None,
            title_id: None,
            parent_binary_id: None,
            platform: "NINTENDO_NES".to_owned(),
        })
        .await
        .unwrap();
    let error = store
        .add_metadata(
            launchable.binary_id,
            MetadataInput {
                key: "meta.score".to_owned(),
                source: "test".to_owned(),
                data: MetadataInputData::Scalar {
                    value: MetadataValue::Number(f64::NAN),
                },
            },
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("finite"));
}

#[tokio::test]
async fn seeded_import_is_atomic_and_only_reports_a_new_root_binary_once() {
    let directory = tempfile::tempdir().unwrap();
    let store = GameCatalog::open(
        directory.path().join("library.db"),
        Arc::new(InMemory::new()),
    )
    .await
    .unwrap();
    let digest = *blake3::hash(b"same-root").as_bytes();
    let input = |name: &str| IdentifiedLaunchable {
        path: directory.path().join(name),
        binary_blake3: digest,
        serial: None,
        title_id: None,
        parent_binary_id: None,
        platform: "NINTENDO_NES".to_owned(),
    };
    let seed = MetadataInput {
        key: "filename.raw".to_owned(),
        source: "Filename".to_owned(),
        data: MetadataInputData::Scalar {
            value: MetadataValue::String("first.rom".to_owned()),
        },
    };
    let (first, created) = store
        .import_launchable_seeded(
            input("first.rom"),
            vec![("host:shiratsu".to_owned(), vec![seed])],
        )
        .await
        .unwrap();
    assert!(created);
    let (_, created) = store
        .import_launchable_seeded(input("second.rom"), Vec::new())
        .await
        .unwrap();
    assert!(!created);
    let metadata = store.metadata(first.binary_id).await.unwrap();
    assert_eq!(metadata.len(), 1);
    assert_eq!(metadata[0].producer, "host:shiratsu");

    let error = store
        .import_launchable_seeded(
            input("broken.rom"),
            vec![(
                "host:bad".to_owned(),
                vec![MetadataInput {
                    key: "invalid..key".to_owned(),
                    source: "test".to_owned(),
                    data: MetadataInputData::Scalar {
                        value: MetadataValue::String("bad".to_owned()),
                    },
                }],
            )],
        )
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("invalid namespaced metadata key")
    );
    assert_eq!(store.games(None).await.unwrap()[0].launchables.len(), 2);
}

#[tokio::test]
async fn active_scrape_job_is_created_and_claimed_once() {
    let directory = tempfile::tempdir().unwrap();
    let store = GameCatalog::open(
        directory.path().join("library.db"),
        Arc::new(InMemory::new()),
    )
    .await
    .unwrap();
    let launchable = store
        .import_launchable(IdentifiedLaunchable {
            path: directory.path().join("game.rom"),
            binary_blake3: *blake3::hash(b"scrape-claim").as_bytes(),
            serial: None,
            title_id: None,
            parent_binary_id: None,
            platform: "NINTENDO_NES".to_owned(),
        })
        .await
        .unwrap();
    let plugins = vec!["fixture".to_owned()];

    let (first, created) = store
        .queue_scrape(launchable.binary_id, launchable.binary_id, &plugins)
        .await
        .unwrap();
    assert!(created);
    let (second, created) = store
        .queue_scrape(launchable.binary_id, launchable.binary_id, &plugins)
        .await
        .unwrap();
    assert!(!created);
    assert_eq!(second.id, first.id);
    assert!(store.claim_scrape_job(first.id).await.unwrap());
    assert!(!store.claim_scrape_job(first.id).await.unwrap());
}
