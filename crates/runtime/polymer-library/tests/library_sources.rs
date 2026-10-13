use std::{path::Path, sync::Arc};

use object_store::memory::InMemory;
use polymer_library::{
    GameCatalog, GameCatalogError, IdentifiedLaunchable, ImportOrigin, LaunchableUri,
};

async fn import(
    catalog: &GameCatalog,
    path: &Path,
    origin: ImportOrigin,
) -> Result<(), GameCatalogError> {
    let input = IdentifiedLaunchable {
        manifest: None,
        path: LaunchableUri::file(path).unwrap(),
        entrypoint: None,
        package: None,
        binary_blake3: *blake3::hash(path.to_string_lossy().as_bytes()).as_bytes(),
        serial: None,
        title_id: None,
        disc_number: None,
        requires_license: false,
        parent_binary_id: None,
        platform: "NINTENDO_NES".to_owned(),
    };
    catalog
        .import_package(input, Vec::new(), origin)
        .await
        .map(|_| ())
}

async fn paths(catalog: &GameCatalog) -> Vec<String> {
    let mut paths: Vec<_> = catalog
        .games()
        .await
        .unwrap()
        .into_iter()
        .flat_map(|game| game.launchables)
        .map(|launchable| {
            launchable
                .identified
                .path
                .source_path()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    paths.sort();
    paths
}

#[tokio::test]
async fn removing_a_source_removes_only_what_it_alone_imported() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = GameCatalog::open(
        &directory.path().join("library.db"),
        Arc::new(InMemory::new()),
    )
    .await
    .unwrap();
    let games = directory.path().join("Games");
    let nested = games.join("nes");
    let sibling = directory.path().join("Games2");

    assert!(matches!(
        import(&catalog, &games.join("outside.nes"), ImportOrigin::Source).await,
        Err(GameCatalogError::Uncovered(_))
    ));

    let outer = catalog.add_source(&games).await.unwrap();
    let inner = catalog.add_source(&nested).await.unwrap();
    assert_eq!(catalog.add_source(&games).await.unwrap(), outer);
    assert!(matches!(
        import(&catalog, &sibling.join("sibling.nes"), ImportOrigin::Source).await,
        Err(GameCatalogError::Uncovered(_))
    ));
    import(&catalog, &games.join("source.nes"), ImportOrigin::Source)
        .await
        .unwrap();
    import(&catalog, &nested.join("shared.nes"), ImportOrigin::Source)
        .await
        .unwrap();
    import(&catalog, &games.join("dropped.nes"), ImportOrigin::Manual)
        .await
        .unwrap();
    import(&catalog, &games.join("both.nes"), ImportOrigin::Source)
        .await
        .unwrap();
    import(&catalog, &games.join("both.nes"), ImportOrigin::Manual)
        .await
        .unwrap();
    import(&catalog, &games.join("both.nes"), ImportOrigin::Source)
        .await
        .unwrap();

    catalog.remove_source(inner.id).await.unwrap();
    assert_eq!(
        paths(&catalog).await,
        ["both.nes", "dropped.nes", "shared.nes", "source.nes"]
    );

    catalog.remove_source(outer.id).await.unwrap();
    assert_eq!(paths(&catalog).await, ["both.nes", "dropped.nes"]);
    assert!(catalog.sources().await.unwrap().is_empty());
    assert!(matches!(
        import(&catalog, &games.join("source.nes"), ImportOrigin::Source).await,
        Err(GameCatalogError::Uncovered(_))
    ));
    import(&catalog, &games.join("dropped.nes"), ImportOrigin::Source)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_source_adopts_a_manual_import_spelled_in_another_case() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = GameCatalog::open(
        &directory.path().join("library.db"),
        Arc::new(InMemory::new()),
    )
    .await
    .unwrap();
    let games = directory.path().join("Games");
    catalog.add_source(&games).await.unwrap();
    let lower = directory.path().join("games").join("Game.nes");
    let input = |path: &Path| IdentifiedLaunchable {
        manifest: None,
        path: LaunchableUri::file(path).unwrap(),
        entrypoint: None,
        package: None,
        binary_blake3: *blake3::hash(b"same").as_bytes(),
        serial: None,
        title_id: None,
        disc_number: None,
        requires_license: false,
        parent_binary_id: None,
        platform: "NINTENDO_NES".to_owned(),
    };
    catalog
        .import_package(input(&lower), Vec::new(), ImportOrigin::Manual)
        .await
        .unwrap();
    catalog
        .import_package(
            input(&games.join("GAME.NES")),
            Vec::new(),
            ImportOrigin::Source,
        )
        .await
        .unwrap();
    let launchables: Vec<_> = catalog
        .games()
        .await
        .unwrap()
        .into_iter()
        .flat_map(|game| game.launchables)
        .collect();
    assert_eq!(launchables.len(), 1);
    catalog
        .remove_source(catalog.sources().await.unwrap()[0].id)
        .await
        .unwrap();
    assert_eq!(catalog.games().await.unwrap().len(), 1);
}
