use polymer_library::firmware::FirmwareStore;
use std::path::PathBuf;

/// Exercises real consolespec definitions without redistributing copyrighted BIOS bytes.
#[test]
#[ignore = "requires POLYMER_TEST_BIOS pointing to a consolespec-known BIOS"]
fn bios_import_persists_and_rejects_modified_content() {
    let source = PathBuf::from(std::env::var_os("POLYMER_TEST_BIOS").expect("BIOS fixture path"));
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let contents = tokio::fs::read(&source).await.unwrap();
        let store = FirmwareStore::open(&root).await.unwrap();
        let installed = store
            .install_bios(&source)
            .await
            .unwrap()
            .expect("known BIOS");
        assert_eq!(
            store.install_bios(&source).await.unwrap().unwrap(),
            installed
        );
        drop(store);

        let store = FirmwareStore::open(&root).await.unwrap();
        let listed = store.list(&installed.platform).await.unwrap();
        assert_eq!(listed, vec![installed.clone()]);
        let (label, path) = store
            .file(
                &installed.platform,
                &installed.firmware_type,
                &installed.label,
            )
            .await
            .unwrap();
        assert_eq!(label, installed.label);
        assert_eq!(tokio::fs::read(path).await.unwrap(), contents);

        let altered = root.join("altered.bin");
        let mut changed = contents;
        changed[0] ^= 0xff;
        tokio::fs::write(&altered, changed).await.unwrap();
        assert!(store.install_bios(&altered).await.unwrap().is_none());
        assert_eq!(
            store.list(&installed.platform).await.unwrap(),
            vec![installed]
        );
    });
}
