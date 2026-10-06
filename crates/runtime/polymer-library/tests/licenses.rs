use std::sync::Arc;

use object_store::memory::InMemory;
use polymer_consolestorage::keys::{KeyIdentity, KeyRecord, KeyWriter};
use polymer_library::{GameCatalog, IdentifiedLaunchable, LaunchableUri, keys::KeyStore};

const CONTENT_ID: &str = "EP9000-NPEA00253_00-GECHOCHROM000001";

#[tokio::test]
async fn titles_needing_a_license_are_licensed_once_their_key_is_imported() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = GameCatalog::open(
        directory.path().join("library.db"),
        Arc::new(InMemory::new()),
    )
    .await
    .unwrap();
    for (file, title_id, requires_license) in
        [("psn", CONTENT_ID, true), ("disc", "BLUS30001", false)]
    {
        catalog
            .import_package(
                IdentifiedLaunchable {
                    manifest: None,
                    path: LaunchableUri::file(directory.path().join(file)).unwrap(),
                    entrypoint: None,
                    package: None,
                    binary_blake3: *blake3::hash(file.as_bytes()).as_bytes(),
                    serial: None,
                    title_id: Some(title_id.to_owned()),
                    disc_number: None,
                    requires_license,
                    parent_binary_id: None,
                    platform: "SONY_PS3".to_owned(),
                },
                Vec::new(),
            )
            .await
            .unwrap();
    }
    let keys = KeyStore::open(directory.path().join("keys"))
        .await
        .unwrap()
        .for_platform("SONY_PS3")
        .unwrap();

    assert!(
        catalog
            .requires_license("SONY_PS3", CONTENT_ID)
            .await
            .unwrap()
    );
    assert!(
        !catalog
            .requires_license("SONY_PS3", "BLUS30001")
            .await
            .unwrap()
    );
    assert!(!keys.licensed(CONTENT_ID).await.unwrap());

    keys.put(&[KeyRecord {
        identity: KeyIdentity {
            namespace: "content".to_owned(),
            id: CONTENT_ID.to_owned(),
        },
        title_id: Some("NPEA00253".to_owned()),
        bytes: vec![0; 16],
    }])
    .await
    .unwrap();
    assert!(keys.licensed(CONTENT_ID).await.unwrap());
    assert!(keys.licensed(&CONTENT_ID.to_lowercase()).await.unwrap());
}
