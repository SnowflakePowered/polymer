use polymer_library::firmware::{FirmwareLookup, FirmwareStore};
use polymer_pluginhost::digest::{DigestAlgorithm, Digestible};
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
        let lookups = installed
            .names
            .iter()
            .cloned()
            .map(FirmwareLookup::Name)
            .chain([
                FirmwareLookup::Md5(source.as_path().digest(DigestAlgorithm::Md5).unwrap()),
                FirmwareLookup::Sha1(source.as_path().digest(DigestAlgorithm::Sha1).unwrap()),
                FirmwareLookup::Sha256(
                    source
                        .as_path()
                        .digest(DigestAlgorithm::Sha2)
                        .unwrap()
                        .to_ascii_uppercase(),
                ),
            ]);
        for lookup in lookups {
            let (label, path) = store.file(&installed.platform, &lookup).await.unwrap();
            assert_eq!(label, installed.label);
            assert_eq!(tokio::fs::read(path).await.unwrap(), contents);
            assert!(store.file("UNKNOWN_PLATFORM", &lookup).await.is_err());
        }
        assert!(
            store
                .file(
                    &installed.platform,
                    &FirmwareLookup::Name("missing BIOS".into())
                )
                .await
                .is_err()
        );

        let after_lookup = store.list(&installed.platform).await.unwrap();
        let altered = root.join("altered.bin");
        let mut changed = contents;
        changed[0] ^= 0xff;
        tokio::fs::write(&altered, changed).await.unwrap();
        assert!(store.install_bios(&altered).await.unwrap().is_none());
        assert_eq!(store.list(&installed.platform).await.unwrap(), after_lookup);
    });
}
