use std::sync::Arc;

use object_store::memory::InMemory;
use polymer_library::{GameCatalog, IdentifiedLaunchable, LaunchableUri, PlatformSettings};

#[tokio::test]
async fn emulator_choices_persist_until_their_game_is_deleted() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("library.db");
    let catalog = GameCatalog::open(&database, Arc::new(InMemory::new()))
        .await
        .unwrap();
    let input = IdentifiedLaunchable {
        manifest: None,
        path: LaunchableUri::file(directory.path().join("chrono-cross.cue")).unwrap(),
        entrypoint: None,
        package: None,
        binary_blake3: *blake3::hash(b"chrono-cross").as_bytes(),
        serial: None,
        title_id: None,
        disc_number: None,
        requires_license: false,
        parent_binary_id: None,
        platform: "SONY_PSX".to_owned(),
    };
    let (launchable, _) = catalog.import_package(input, Vec::new()).await.unwrap();
    catalog
        .set_launchable_emulator(launchable.id, Some("duckstation"))
        .await
        .unwrap();
    catalog
        .save_platform_settings(
            "SONY_PSX",
            &PlatformSettings {
                emulator: Some("ares".to_owned()),
            },
        )
        .await
        .unwrap();

    drop(catalog);
    let catalog = GameCatalog::open(&database, Arc::new(InMemory::new()))
        .await
        .unwrap();
    assert_eq!(
        catalog
            .launchable_emulator(launchable.id)
            .await
            .unwrap()
            .as_deref(),
        Some("duckstation")
    );
    assert_eq!(
        catalog.platform_settings().await.unwrap()["SONY_PSX"]
            .emulator
            .as_deref(),
        Some("ares")
    );

    catalog
        .set_launchable_emulator(launchable.id, None)
        .await
        .unwrap();
    assert_eq!(
        catalog.launchable_emulator(launchable.id).await.unwrap(),
        None
    );
    catalog
        .set_launchable_emulator(launchable.id, Some("duckstation"))
        .await
        .unwrap();
    catalog.delete_game(launchable.binary_id).await.unwrap();
    assert_eq!(
        catalog.launchable_emulator(launchable.id).await.unwrap(),
        None
    );
    assert_eq!(
        catalog.platform_settings().await.unwrap()["SONY_PSX"]
            .emulator
            .as_deref(),
        Some("ares")
    );
}
