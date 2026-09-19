use polymer_pluginhost::{
    loading::PluginRegistry,
    runtime::{PluginRuntime, PluginRuntimeOptions},
};
use polymer_scraping::Package;
use std::collections::BTreeMap;

#[tokio::test(flavor = "current_thread")]
async fn package_mounts_images_directories_and_cues_read_only() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("game.iso"), b"game").unwrap();
    std::fs::write(
        root.path().join("plugin.toml"),
        r#"
        [plugin]
        id = "worker-test"
        version = "1"
        class = "emulator"
        main = "plugin.js"
        [emulator]
        platforms = ["TEST"]
        [emulator.configuration]
        definitions = ["config.toml"]
    "#,
    )
    .unwrap();
    std::fs::write(
        root.path().join("config.toml"),
        r#"
        [targets.settings]
        output = { format = "ini", path = "settings.ini" }
        [input]
        driver = "sdl3"
        device = "SDL-{index}"
        [input.mapping]
        [input.layouts]
        [platforms.TEST.ports]
    "#,
    )
    .unwrap();
    std::fs::write(root.path().join("plugin.js"), r#"
        export function onLaunch(args) {
            // Callback arguments cannot redirect the host-bound package mount.
            args.launchPath = "not-the-game";
            args.package = {};
            const gamePath = args.filesystem.mountPackage("/Game");
            args.filesystem.mountFile("/Emulator/emulator.exe", new Uint8Array([1]), { mode: "rx" });
            return { command: args.filesystem.path("/Emulator/emulator.exe"), argv: [gamePath],
                hooks: { graphics: ["vulkan"], input: [] }, surfaces: [{ index: 0 }] };
        }
    "#).unwrap();
    let registry = PluginRegistry::new();
    registry.load(root.path()).unwrap();
    let storage = tempfile::tempdir().unwrap();
    let runtime = PluginRuntime::new(
        registry,
        polymer_library::Directories::rooted(storage.path(), storage.path()),
        PluginRuntimeOptions::default(),
    );
    // A minimal Games Express PC Engine CD track recognized by Gumshoe.
    // Use an original filename that must be rewritten for the mounted bundle.
    let mut track = vec![0; 17 * 2048];
    track[16 * 2048 + 8..16 * 2048 + 28].copy_from_slice(b"HACKER CD ROM SYSTEM");
    std::fs::write(root.path().join("original track.bin"), &track).unwrap();
    let source = root.path().join("source.cue");
    std::fs::write(
        &source,
        "FILE \"original track.bin\" BINARY\n  TRACK 01 MODE1/2048\n    INDEX 01 00:00:00\n",
    )
    .unwrap();
    let identified = Package::identify(&source, Some("NEC_TG16"), None).unwrap();
    // Simulate the frontend round trip that drops the private CUE manifest.
    let mut package: Package =
        serde_json::from_value(serde_json::to_value(identified).unwrap()).unwrap();
    package.prepare_launch().unwrap();
    assert_eq!(
        package.binary_digest,
        blake3::hash(&track).to_hex().as_str()
    );

    // Prepared packages retain their bundle and identity across another round
    // trip without reopening the original descriptor.
    let mut package: Package =
        serde_json::from_value(serde_json::to_value(package).unwrap()).unwrap();
    std::fs::remove_file(&source).unwrap();
    package.prepare_launch().unwrap();
    let cue = "FILE \"file-0000.bin\" BINARY\n  TRACK 01 MODE1/2048\n    INDEX 01 00:00:00\n";
    let result = runtime
        .handle()
        .invoke_with_storage_owned(
            "worker-test",
            &["onLaunch".to_owned()],
            vec![serde_json::json!({
                "launchPath": root.path().join("source.cue"), "console": "TEST",
                "controllerTopology": { "mappings": [] }, "virtualize": true,
                "package": package,
            })],
            BTreeMap::new(),
        )
        .await
        .unwrap();
    assert!(
        result.value["argv"][0]
            .as_str()
            .unwrap()
            .ends_with("disc.cue")
    );
    let prepared = result.prepared_filesystem.unwrap();
    let filesystem = prepared.filesystem();
    filesystem.materialize().unwrap();
    for (name, expected) in [
        ("disc.cue", cue.as_bytes()),
        ("file-0000.bin", track.as_slice()),
    ] {
        let path = format!("/Game/{name}").parse().unwrap();
        let file = filesystem
            .open(
                &path,
                polymer_vfs::OpenOptions {
                    read: true,
                    write: false,
                    truncate: false,
                },
            )
            .unwrap();
        let mut bytes = vec![0; expected.len()];
        let length = file.read(0, &mut bytes).unwrap();
        assert_eq!(&bytes[..length], expected);
        assert!(
            filesystem
                .open(
                    &path,
                    polymer_vfs::OpenOptions {
                        read: true,
                        write: true,
                        truncate: false
                    }
                )
                .is_err()
        );
    }
    assert!(
        filesystem
            .open(
                &"/Game/game.iso".parse().unwrap(),
                polymer_vfs::OpenOptions {
                    read: true,
                    write: false,
                    truncate: false
                }
            )
            .is_err()
    );
    std::fs::write(root.path().join("standalone.iso"), b"game").unwrap();
    std::fs::write(root.path().join("rom.gba"), b"cartridge").unwrap();
    std::fs::create_dir_all(root.path().join("extracted/code")).unwrap();
    std::fs::write(root.path().join("extracted/code/main.rpx"), b"executable").unwrap();
    for (source, entrypoint, member, expected) in [
        (
            "standalone.iso",
            "/Game/standalone.iso",
            "/Game/standalone.iso",
            b"game".as_slice(),
        ),
        (
            "rom.gba",
            "/Game/rom.gba",
            "/Game/rom.gba",
            b"cartridge".as_slice(),
        ),
        (
            "extracted",
            "/Game",
            "/Game/code/main.rpx",
            b"executable".as_slice(),
        ),
    ] {
        let result = runtime
            .handle()
            .invoke_with_storage_owned(
                "worker-test",
                &["onLaunch".to_owned()],
                vec![serde_json::json!({
                    "launchPath": root.path().join(source), "console": "TEST",
                    "package": {}, "controllerTopology": { "mappings": [] },
                })],
                BTreeMap::new(),
            )
            .await
            .unwrap();
        assert!(
            result.value["argv"][0]
                .as_str()
                .unwrap()
                .replace('\\', "/")
                .ends_with(entrypoint)
        );
        let prepared = result.prepared_filesystem.unwrap();
        let filesystem = prepared.filesystem();
        filesystem.materialize().unwrap();
        let path = member.parse().unwrap();
        let file = filesystem
            .open(
                &path,
                polymer_vfs::OpenOptions {
                    read: true,
                    write: false,
                    truncate: false,
                },
            )
            .unwrap();
        let mut bytes = [0; 256];
        let length = file.read(0, &mut bytes).unwrap();
        assert_eq!(&bytes[..length], expected);
        assert!(
            filesystem
                .open(
                    &path,
                    polymer_vfs::OpenOptions {
                        read: true,
                        write: true,
                        truncate: false
                    }
                )
                .is_err()
        );
        assert!(
            filesystem
                .open(
                    &"/Game/plugin.toml".parse().unwrap(),
                    polymer_vfs::OpenOptions {
                        read: true,
                        write: false,
                        truncate: false
                    }
                )
                .is_err()
        );
    }
    let missing = runtime
        .handle()
        .invoke_with_storage_owned(
            "worker-test",
            &["onLaunch".to_owned()],
            vec![serde_json::json!({
                "launchPath": root.path().join("missing.iso"), "console": "TEST",
                "controllerTopology": { "mappings": [] },
            })],
            BTreeMap::new(),
        )
        .await;
    assert!(missing.is_err());
}
