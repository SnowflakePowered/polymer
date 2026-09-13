use super::*;

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
    let identified = Package::from_path(&path).unwrap();
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
async fn reimport_enriches_existing_games_with_raw_file_hashes() {
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
    // Simulate a library imported before dump hashes were available.
    catalog
        .replace_metadata(original.binary_id, "host:raw-file", Vec::new())
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
    assert!(metadata.iter().any(|value| value.key == "package.hash.sha1"
        && value.data
            == polymer_library::MetadataData::Scalar(MetadataValue::String(
                "36aac0dfa63779b45bd8eef054c45730f84b6a3b".to_owned()
            ))));
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
    assert_eq!(package.binary_digest, expected);
    let raw = &package.import.as_ref().unwrap().raw_file;
    let values = raw
        .iter()
        .map(|metadata| {
            let MetadataInputData::Scalar {
                value: MetadataValue::String(value),
            } = &metadata.data
            else {
                panic!("raw file fingerprints must be scalar strings");
            };
            (metadata.key.as_str(), value.as_str())
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    // These digests include the iNES header, unlike the normalized identity.
    assert_eq!(
        values["package.hash.sha1"],
        "36aac0dfa63779b45bd8eef054c45730f84b6a3b"
    );
    assert_eq!(
        values["package.hash.md5"],
        "13d8046bee1856d51bbfa1a003d1b370"
    );
    assert_eq!(values["package.hash.crc32"], "0fc878e3");
    assert_eq!(values["package.size"], "48");
    assert!(raw.iter().all(|metadata| metadata.source == raw[0].source));
    assert!(matches!(
        Package::identify(&path, Some("NINTENDO_SNES"), None),
        Err(crate::ScrapingError::PlatformMismatch { .. })
    ));
    let json = serde_json::to_value(package).unwrap();
    assert_eq!(json["binaryDigest"], expected);
    assert!(json.get("import").is_none());
}
