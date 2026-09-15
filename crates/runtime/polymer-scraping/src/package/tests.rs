use super::*;
use polymer_library::GameCatalog;
use polymer_pluginhost::digest::{DigestAlgorithm, Digestible, PackageBinary};

#[tokio::test(flavor = "current_thread")]
async fn registered_package_does_not_reopen_game_file() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("registered.nes");
    let mut image = vec![0; 16];
    image[..4].copy_from_slice(b"NES\x1a");
    image.extend_from_slice(&[0x42; 32]);
    std::fs::write(&path, image).unwrap();
    let catalog = GameCatalog::open(
        directory.path().join("library.db"),
        std::sync::Arc::new(object_store::memory::InMemory::new()),
    )
    .await
    .unwrap();
    let mut identified = Package::from_path(&path).unwrap();
    identified.ensure_binary_identity().unwrap();
    let (registered, _) = identified.clone().import(&catalog).await.unwrap();
    std::fs::remove_file(&path).unwrap();

    let stored = catalog.launchable(registered.id).await.unwrap();
    let package = Package::from_launchable(stored).unwrap();
    assert_eq!(package.path, identified.path);
    assert_eq!(
        serde_json::to_value(&package).unwrap(),
        serde_json::to_value(&identified).unwrap()
    );
    assert!(package.import(&catalog).await.is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn reimport_clears_legacy_hash_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("legacy.nes");
    let mut image = vec![0; 16];
    image[..4].copy_from_slice(b"NES\x1a");
    image.extend_from_slice(&[0x42; 32]);
    std::fs::write(&path, image).unwrap();
    let catalog = GameCatalog::open(
        directory.path().join("library.db"),
        std::sync::Arc::new(object_store::memory::InMemory::new()),
    )
    .await
    .unwrap();
    let (original, created) = Package::from_path(&path)
        .unwrap()
        .import(&catalog)
        .await
        .unwrap();
    assert!(created);
    // Simulate eager hash metadata retained from an older import.
    catalog
        .replace_metadata(
            original.binary_id,
            "host:raw-file",
            vec![Package::scalar("package.hash.sha1", "Legacy dump", "stale")],
        )
        .await
        .unwrap();
    let (enriched, created) = Package::from_path(&path)
        .unwrap()
        .import(&catalog)
        .await
        .unwrap();
    assert!(!created);
    assert_eq!(enriched.id, original.id);
    let metadata = catalog.metadata(original.binary_id).await.unwrap();
    assert!(
        metadata
            .iter()
            .all(|value| !value.key.starts_with("package.hash."))
    );
}

#[test]
fn package_uses_the_normalized_gumshoe_binary_digest() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("headered.nes");
    let payload = [0x42; 32];
    let mut image = vec![0; 16];
    image[..4].copy_from_slice(b"NES\x1a");
    image.extend_from_slice(&payload);
    std::fs::write(&path, image).unwrap();

    let package = Package::from_path(&path).unwrap();
    let expected = blake3::hash(&payload).to_hex().to_string();

    assert_eq!(package.platform, "NINTENDO_NES");
    assert!(
        package.binary_digest.is_empty(),
        "identification must leave the digest lazy"
    );
    assert_eq!(
        PackageBinary(&path)
            .digest(DigestAlgorithm::Blake3)
            .unwrap(),
        expected
    );
    assert_eq!(
        path.as_path().digest(DigestAlgorithm::Sha1).unwrap(),
        "36aac0dfa63779b45bd8eef054c45730f84b6a3b"
    );
    assert_eq!(
        path.as_path().digest(DigestAlgorithm::Md5).unwrap(),
        "13d8046bee1856d51bbfa1a003d1b370"
    );
    assert_eq!(
        path.as_path().digest(DigestAlgorithm::Crc32).unwrap(),
        "0fc878e3"
    );
    let files = package.scrape_value();
    assert_eq!(files["files"][0]["size"], 48);
    assert!(matches!(
        Package::identify(&path, Some("NINTENDO_SNES"), None),
        Err(crate::ScrapingError::PlatformMismatch { .. })
    ));
    let json = serde_json::to_value(package).unwrap();
    assert_eq!(json["binaryDigest"], "");
    assert!(json.get("import").is_none());
}
