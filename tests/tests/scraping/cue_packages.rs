use polymer_library::GameCatalog;
use polymer_scraping::{Ingestor, Package};
use std::{path::PathBuf, sync::Arc};

/// Read-only integration against local dumps, never checked into the repository.
/// Set POLYMER_CUE_DIRECTORY to a directory containing one complete BIN/CUE disc.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires POLYMER_CUE_DIRECTORY with a complete local disc"]
async fn complete_disc_identifies_imports_and_prepares_without_track_launchables() {
    let path =
        PathBuf::from(std::env::var_os("POLYMER_CUE_DIRECTORY").expect("POLYMER_CUE_DIRECTORY"));
    let directory = tempfile::tempdir().unwrap();
    let catalog = Arc::new(
        GameCatalog::open(
            directory.path().join("library.db"),
            Arc::new(object_store::memory::InMemory::new()),
        )
        .await
        .unwrap(),
    );
    let mut identified = Ingestor::identify(&path, None, None).unwrap();
    assert_eq!(identified.len(), 1);
    let from_directory = identified.pop().unwrap();
    assert!(
        from_directory
            .path
            .extension()
            .unwrap()
            .eq_ignore_ascii_case("cue")
    );
    let explicit = Package::from_path(&from_directory.path).unwrap();
    assert_eq!(explicit.path, from_directory.path);
    let (first, _) = from_directory.import(&catalog).await.unwrap();
    let (same, created) = explicit.import(&catalog).await.unwrap();
    assert_eq!(same.id, first.id);
    assert!(!created);
    let stored = catalog.launchable(first.id).await.unwrap();
    let manifest = stored.manifest.as_ref().unwrap();
    assert!(
        manifest.template.is_some(),
        "import must persist a prepared descriptor"
    );
    for member in &manifest.files {
        if member.name.to_ascii_lowercase().ends_with(".bin") {
            assert!(
                Package::from_path(&member.path).is_err(),
                "a track must not identify independently: {}",
                member.name
            );
        }
    }
    let expected_members = manifest.files.len();
    let mut package = Package::from_launchable(stored).unwrap();
    package.prepare_launch().unwrap();
    let bundle = package.bundle.unwrap();
    assert_eq!(bundle.files.len(), expected_members + 1);
    let rendered = bundle
        .files
        .iter()
        .find_map(|file| file.text.as_deref())
        .unwrap();
    let cue = cuna::CueSheet::new(rendered).unwrap();
    assert_eq!(cue.files.len(), expected_members);
    let games = catalog.games(None).await.unwrap();
    assert_eq!(games.len(), 1);
    assert_eq!(games[0].launchables.len(), 1);
    println!(
        "Identified {}: {} members, serial {:?}",
        package.platform, expected_members, package.serial
    );
}
