use super::*;

impl FirmwareInstallDescriptor {
    fn fixture(platform: &str, label: &str, bytes: &[u8]) -> FirmwareInstallDescriptor {
        FirmwareInstallDescriptor {
            platform: platform.into(),
            firmware_type: "bios".into(),
            label: label.into(),
            contents: bytes.into(),
        }
    }
}

#[tokio::test]
async fn replacement_is_atomic_scoped_and_survives_reopen() {
    let root = tempfile::tempdir().unwrap();
    let platforms = vec!["SONY_PSX".into(), "SONY_PS2".into()];
    let store = FirmwareStore::open(root.path()).await.unwrap();
    let installed = store
        .install(
            &[
                FirmwareInstallDescriptor::fixture("SONY_PSX", "../Japanese BIOS", b"original"),
                FirmwareInstallDescriptor::fixture(
                    "SONY_PS2",
                    "../Japanese BIOS",
                    b"other platform",
                ),
            ],
            &platforms,
        )
        .await
        .unwrap();
    let first = &installed[0];
    let (_, original_path) = store
        .file("SONY_PSX", &first.firmware_type, &first.label)
        .await
        .unwrap();
    let (_, other_path) = store
        .file("SONY_PS2", &first.firmware_type, &first.label)
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read(other_path).await.unwrap(),
        b"other platform"
    );
    let replaced = store
        .install(
            &[FirmwareInstallDescriptor::fixture(
                "SONY_PSX",
                "../Japanese BIOS",
                b"replacement",
            )],
            &platforms,
        )
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(original_path).await.unwrap(), b"original");

    let invalid = vec![
        FirmwareInstallDescriptor::fixture("SONY_PSX", "../Japanese BIOS", b"partial"),
        FirmwareInstallDescriptor::fixture("UNDECLARED", "key", b"bad"),
    ];
    assert!(store.install(&invalid, &platforms).await.is_err());
    drop(store);
    let store = FirmwareStore::open(root.path()).await.unwrap();
    assert_eq!(store.list("SONY_PSX").await.unwrap(), replaced);
    let (_, path) = store
        .file("SONY_PSX", &first.firmware_type, &first.label)
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(path).await.unwrap(), b"replacement");
    assert_eq!(store.list("SONY_PS2").await.unwrap(), installed[1..]);
}

#[tokio::test]
async fn failed_staging_and_duplicate_slots_preserve_existing_batch() {
    let root = tempfile::tempdir().unwrap();
    let store = FirmwareStore::open(root.path()).await.unwrap();
    let platforms = vec!["SONY_PSX".into(), "SONY_PS2".into()];
    let batch = vec![FirmwareInstallDescriptor::fixture(
        "SONY_PSX", "BIOS", b"one",
    )];
    let original = store.install(&batch, &platforms).await.unwrap();
    assert_eq!(store.install(&batch, &platforms).await.unwrap(), original);
    assert!(
        store
            .install(&[batch[0].clone(), batch[0].clone()], &platforms)
            .await
            .is_err()
    );
    // Make staging the second platform fail after the first content has been staged.
    tokio::fs::write(root.path().join("SONY_PS2"), b"not a directory")
        .await
        .unwrap();
    assert!(
        store
            .install(
                &[
                    FirmwareInstallDescriptor::fixture("SONY_PSX", "BIOS", b"two"),
                    FirmwareInstallDescriptor::fixture("SONY_PS2", "BIOS", b"three")
                ],
                &platforms
            )
            .await
            .is_err()
    );
    assert_eq!(store.list("SONY_PSX").await.unwrap(), original);
    let (_, path) = store
        .file("SONY_PSX", &original[0].firmware_type, &original[0].label)
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(path).await.unwrap(), b"one");
    assert!(store.list("SONY_PS2").await.unwrap().is_empty());
}
