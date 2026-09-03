use super::*;
use object_store::{ObjectStoreExt, memory::InMemory};

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
            MetadataInput::Object {
                relation: Relation::BoxartFront,
                language: Some("en-US".to_owned()),
                source: "test".to_owned(),
                media_type: "image/png".to_owned(),
                path: source.clone(),
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
            MetadataInput::ReleaseYear {
                language: None,
                source: "test".to_owned(),
                year: 1987,
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
