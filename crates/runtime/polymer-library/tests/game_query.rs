use std::{path::Path, sync::Arc};

use object_store::memory::InMemory;
use polymer_library::{
    GameCatalog, GameQuery, GameSort, IdentifiedLaunchable, LaunchableUri, MetadataInput,
    MetadataInputData, MetadataValue,
};
use uuid::Uuid;

async fn import(
    catalog: &GameCatalog,
    directory: &Path,
    file: &str,
    platform: &str,
    parent: Option<Uuid>,
    seeds: &[(&str, &str)],
) -> Uuid {
    let input = IdentifiedLaunchable {
        manifest: None,
        path: LaunchableUri::file(directory.join(file)).unwrap(),
        entrypoint: None,
        package: None,
        binary_blake3: *blake3::hash(file.as_bytes()).as_bytes(),
        serial: None,
        title_id: None,
        disc_number: None,
        requires_license: false,
        parent_binary_id: parent,
        platform: platform.to_owned(),
    };
    let seeds = seeds
        .iter()
        .map(|(key, value)| MetadataInput {
            key: (*key).to_owned(),
            source: "test".to_owned(),
            data: MetadataInputData::Scalar {
                value: MetadataValue::String((*value).to_owned()),
            },
        })
        .collect();
    let (launchable, _) = catalog
        .import_package(input, vec![("seed".to_owned(), seeds)])
        .await
        .unwrap();
    launchable.binary_id
}

async fn titles(catalog: &GameCatalog, query: GameQuery) -> (Vec<String>, usize) {
    let page = catalog.query_games(query).await.unwrap();
    (
        page.games.into_iter().map(|game| game.title).collect(),
        page.total,
    )
}

fn query(text: Option<&str>, platforms: &[&str], sort: GameSort) -> GameQuery {
    GameQuery {
        text: text.map(str::to_owned),
        platforms: platforms
            .iter()
            .map(|platform| (*platform).to_owned())
            .collect(),
        sort,
        descending: false,
        offset: 0,
        limit: 10,
        metadata_keys: None,
    }
}

#[tokio::test]
async fn titles_follow_catalog_writes() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("library.db");
    let catalog = GameCatalog::open(&database, Arc::new(InMemory::new()))
        .await
        .unwrap();
    let final_fantasy = import(
        &catalog,
        directory.path(),
        "ff7-1.bin",
        "SONY_PSX",
        None,
        &[("meta.title", "Final Fantasy VII")],
    )
    .await;
    import(
        &catalog,
        directory.path(),
        "ff7-2.bin",
        "SONY_PSX",
        Some(final_fantasy),
        &[("meta.title", "Disc Two")],
    )
    .await;
    let chrono = import(
        &catalog,
        directory.path(),
        "chrono.sfc",
        "NINTENDO_SNES",
        None,
        &[("package.internalname", "CHRONO TRIGGER")],
    )
    .await;
    import(
        &catalog,
        directory.path(),
        "zelda.sfc",
        "NINTENDO_SNES",
        None,
        &[],
    )
    .await;
    import(
        &catalog,
        directory.path(),
        "zelda.sfc",
        "NINTENDO_SNES",
        None,
        &[],
    )
    .await;

    assert_eq!(
        titles(&catalog, query(None, &[], GameSort::Title)).await,
        (
            vec![
                "CHRONO TRIGGER".to_owned(),
                "Final Fantasy VII".to_owned(),
                "zelda.sfc".to_owned()
            ],
            3
        )
    );
    assert_eq!(
        titles(&catalog, query(None, &["NINTENDO_SNES"], GameSort::Added)).await,
        (vec!["CHRONO TRIGGER".to_owned(), "zelda.sfc".to_owned()], 2)
    );
    assert_eq!(
        titles(&catalog, query(Some("ffvii"), &[], GameSort::Title))
            .await
            .0[0],
        "Final Fantasy VII"
    );
    assert_eq!(
        titles(
            &catalog,
            query(Some("ffvii"), &["NINTENDO_SNES"], GameSort::Title)
        )
        .await
        .1,
        0
    );

    catalog
        .add_metadata(
            chrono,
            MetadataInput {
                key: "meta.title".to_owned(),
                source: "manual".to_owned(),
                data: MetadataInputData::Scalar {
                    value: MetadataValue::String("Chrono Trigger".to_owned()),
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(
        titles(&catalog, query(Some("chrono trig"), &[], GameSort::Title)).await,
        (vec!["Chrono Trigger".to_owned()], 1)
    );

    catalog.delete_game(final_fantasy).await.unwrap();
    assert_eq!(
        titles(&catalog, query(Some("ffvii"), &[], GameSort::Title))
            .await
            .1,
        0
    );
    assert_eq!(
        titles(&catalog, query(None, &[], GameSort::Title)).await.1,
        2
    );

    drop(catalog);
    let catalog = GameCatalog::open(&database, Arc::new(InMemory::new()))
        .await
        .unwrap();
    assert_eq!(
        titles(&catalog, query(Some("zelda"), &[], GameSort::Title)).await,
        (vec!["zelda.sfc".to_owned()], 1)
    );
    assert_eq!(
        catalog.game_platforms().await.unwrap(),
        vec!["NINTENDO_SNES".to_owned()]
    );
}
