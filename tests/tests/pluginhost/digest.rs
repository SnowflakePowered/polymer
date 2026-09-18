use polymer_pluginhost::digest::{DigestAlgorithm, Digestible, PackageBinary};

#[test]
fn raw_files_and_normalized_binaries_have_distinct_digests() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("headered.nes");
    let mut image = vec![0; 16];
    image[..4].copy_from_slice(b"NES\x1a");
    image.extend_from_slice(b"123456789");
    std::fs::write(&path, &image).unwrap();
    let payload = directory.path().join("payload");
    std::fs::write(&payload, b"123456789").unwrap();
    let vectors = [
        (
            DigestAlgorithm::Sha2,
            "15e2b0d3c33891ebb0f1ef609ec419420c20e320ce94c65fbc8c3312448eb225",
        ),
        (
            DigestAlgorithm::Sha1,
            "f7c3bc1d808e04732adf679965ccc34ca7ae3441",
        ),
        (DigestAlgorithm::Md5, "25f9e794323b453885f5181f1b624d0b"),
        (DigestAlgorithm::Crc32, "cbf43926"),
    ];
    for (algorithm, expected) in vectors {
        assert_eq!(payload.as_path().digest(algorithm).unwrap(), expected);
        assert_eq!(PackageBinary(&path).digest(algorithm).unwrap(), expected);
        assert_ne!(path.as_path().digest(algorithm).unwrap(), expected);
    }
    assert_eq!(
        PackageBinary(&path)
            .digest(DigestAlgorithm::Blake3)
            .unwrap(),
        blake3::hash(b"123456789").to_hex().as_str()
    );
    // Repeated requests observe current bytes rather than a cached digest.
    std::fs::write(&payload, b"changed").unwrap();
    assert_ne!(
        payload.as_path().digest(DigestAlgorithm::Crc32).unwrap(),
        "cbf43926"
    );
}
