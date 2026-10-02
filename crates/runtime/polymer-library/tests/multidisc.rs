use std::{path::Path, sync::Arc};

use object_store::memory::InMemory;
use polymer_library::{
    GameCatalog, GameCatalogError, IdentifiedLaunchable, Launchable, LaunchableUri,
};

const EXECUTABLE: &[u8] = b"shared Chrono Cross executable";

async fn import(
    catalog: &GameCatalog,
    directory: &Path,
    file: &str,
    serial: &str,
) -> Result<Launchable, GameCatalogError> {
    let input = IdentifiedLaunchable {
        manifest: None,
        path: LaunchableUri::file(directory.join(file)).unwrap(),
        entrypoint: None,
        package: None,
        binary_blake3: *blake3::hash(EXECUTABLE).as_bytes(),
        serial: Some(serial.to_owned()),
        title_id: None,
        disc_number: None,
        parent_binary_id: None,
        platform: "SONY_PSX".to_owned(),
    };
    catalog
        .import_package(input, Vec::new())
        .await
        .map(|(launchable, _)| launchable)
}

#[tokio::test]
async fn discs_sharing_an_executable_join_their_first_disc() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = GameCatalog::open(
        directory.path().join("library.db"),
        Arc::new(InMemory::new()),
    )
    .await
    .unwrap();

    let early = import(
        &catalog,
        directory.path(),
        "chrono-cross-2.cue",
        "SLUS-01080",
    )
    .await;
    assert!(matches!(
        early,
        Err(GameCatalogError::MissingFirstDisc { disc: 2, .. })
    ));

    let first = import(
        &catalog,
        directory.path(),
        "chrono-cross-1.cue",
        "SLUS-01041",
    )
    .await
    .unwrap();
    let second = import(
        &catalog,
        directory.path(),
        "chrono-cross-2.cue",
        "SLUS-01080",
    )
    .await
    .unwrap();
    assert_ne!(first.binary_id, second.binary_id);
    assert_eq!(second.identified.parent_binary_id, Some(first.binary_id));

    let game = catalog.game(first.binary_id).await.unwrap().unwrap();
    let serials = game
        .launchables
        .iter()
        .map(|launchable| launchable.identified.serial.as_deref())
        .collect::<Vec<_>>();
    assert_eq!(serials, [Some("SLUS-01041"), Some("SLUS-01080")]);
}
