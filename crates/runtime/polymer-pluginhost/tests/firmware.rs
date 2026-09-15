use polymer_library::{
    Directories, StorageKind, StorageManager,
    firmware::{FirmwareInstallDescriptor, FirmwareStore},
};
use polymer_pluginhost::{
    loading::PluginRegistry,
    platform::Device,
    runtime::{PluginRuntime, PluginRuntimeOptions},
};
use serde_json::json;
use std::{collections::BTreeMap, path::PathBuf};

struct Fixture {
    root: tempfile::TempDir,
    directories: Directories,
    registry: PluginRegistry,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        Self {
            directories: Directories::rooted(root.path(), root.path()),
            root,
            registry: PluginRegistry::new(),
        }
    }

    fn plugin(&self, id: &str, platform: &str, code: &str, emulator: bool) {
        let path = self.root.path().join(id);
        std::fs::create_dir_all(&path).unwrap();
        let class = if emulator { "emulator" } else { "platform" };
        let mut manifest = format!(
            r#"
            [plugin]
            id = "{id}"
            version = "1"
            class = "{class}"
            main = "main.js"
            [{class}]
            platforms = ["{platform}"]
        "#
        );
        if emulator {
            manifest.push_str("\n[emulator.configuration]\ndefinitions = [\"config.toml\"]\n");
            std::fs::write(
                path.join("config.toml"),
                format!(
                    r#"
                [targets.settings]
                output = {{ format = "ini", path = "settings.ini" }}
                [input]
                driver = "sdl3"
                device = "SDL-{{index}}"
                [input.mapping]
                [input.layouts]
                [platforms.{platform}.ports]
            "#
                ),
            )
            .unwrap();
        }
        std::fs::write(path.join("plugin.toml"), manifest).unwrap();
        std::fs::write(path.join("main.js"), code).unwrap();
        self.registry.load(path).unwrap();
    }

    fn runtime(&self) -> PluginRuntime {
        PluginRuntime::new(
            self.registry.clone(),
            self.directories.clone(),
            PluginRuntimeOptions::default(),
        )
    }

    fn input(&self) -> PathBuf {
        let path = self.root.path().join("hardware.bin");
        std::fs::write(&path, b"abc").unwrap();
        path
    }
}

#[tokio::test]
async fn recognition_continues_past_errors_and_preserves_typed_array_views() {
    let fixture = Fixture::new();
    // Register in reverse order to exercise the registry's ID ordering.
    fixture.plugin("40-later", "SONY_PS2", "export function onCheckFirmware() { return [{ platform: 'SONY_PS2', type: 'bios', label: 'wrong' }]; }", false);
    fixture.plugin("30-match", "SONY_PSX", r#"
        export default { async onCheckFirmware(file) {
            if (file.name !== 'hardware.bin' || await file.text() !== 'abc') throw Error('input');
            if (await file.digest('sha2') !== 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad') throw Error('digest');
            const bytes = await file.bytes(); bytes[0] = 0;
            if (await file.text() !== 'abc') throw Error('mutable input');
            return [
                { platform: 'SONY_PSX', type: 'bios', label: 'Original' },
                { platform: 'SONY_PSX', type: 'key', label: 'Extracted', contents: new Uint8Array([9, 2, 3, 9]).subarray(1, 3) },
            ];
        } };
    "#, false);
    fixture.plugin("20-invalid", "NINTENDO_NES", "export function onCheckFirmware() { return [{ platform: 'SONY_PSX', type: 'bios', label: 'undeclared' }]; }", false);
    fixture.plugin(
        "10-error",
        "NINTENDO_SNES",
        "export function onCheckFirmware() { throw Error('recognizer broke'); }",
        false,
    );
    fixture.plugin(
        "00-absent",
        "NINTENDO_GBA",
        "export const unrelated = 1;",
        false,
    );
    let runtime = fixture.runtime();
    let result = runtime
        .handle()
        .install_firmware(&fixture.input())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.len(), 2);
    let store = FirmwareStore::open(fixture.directories.firmware())
        .await
        .unwrap();
    for (description, expected) in result.iter().zip([b"abc".as_slice(), &[2, 3]]) {
        let (_, path) = store
            .file("SONY_PSX", &description.firmware_type, &description.label)
            .await
            .unwrap();
        assert_eq!(std::fs::read(path).unwrap(), expected);
    }
    assert!(store.list("SONY_PS2").await.unwrap().is_empty());
    assert_eq!(
        runtime
            .handle()
            .install_firmware(&fixture.input())
            .await
            .unwrap()
            .unwrap(),
        result
    );
}

#[tokio::test]
async fn invalid_batch_is_discarded_and_no_match_is_optional() {
    let fixture = Fixture::new();
    fixture.plugin(
        "00-null",
        "NINTENDO_GBA",
        "export function onCheckFirmware() { return null; }",
        false,
    );
    fixture.plugin(
        "01-empty",
        "NINTENDO_SNES",
        "export async function onCheckFirmware() { return []; }",
        false,
    );
    fixture.plugin(
        "02-undefined",
        "NINTENDO_NES",
        "export function onCheckFirmware() {}",
        false,
    );
    let runtime = fixture.runtime();
    assert!(
        runtime
            .handle()
            .install_firmware(&fixture.input())
            .await
            .unwrap()
            .is_none()
    );
    fixture.plugin(
        "03-invalid",
        "SONY_PSX",
        r#"
        export function onCheckFirmware() { return [
            { platform: 'SONY_PSX', type: 'bios', label: 'Must not install' },
            { platform: 'SONY_PSX', type: 'key', label: 'Invalid bytes', contents: [1, 2] },
        ]; }
    "#,
        false,
    );
    let error = runtime
        .handle()
        .install_firmware(&fixture.input())
        .await
        .unwrap_err();
    assert!(error.message.contains("03-invalid"));
    let store = FirmwareStore::open(fixture.directories.firmware())
        .await
        .unwrap();
    assert!(store.list("SONY_PSX").await.unwrap().is_empty());
}

#[tokio::test]
async fn launch_files_are_scoped_read_only_and_keep_their_content_after_replacement() {
    let fixture = Fixture::new();
    let store = FirmwareStore::open(fixture.directories.firmware())
        .await
        .unwrap();
    store
        .install(
            &[
                FirmwareInstallDescriptor {
                    platform: "SONY_PSX".into(),
                    firmware_type: "bios".into(),
                    label: "BIOS".into(),
                    contents: b"abc".as_slice().into(),
                },
                FirmwareInstallDescriptor {
                    platform: "SONY_PS2".into(),
                    firmware_type: "bios".into(),
                    label: "Other".into(),
                    contents: b"other".as_slice().into(),
                },
            ],
            &["SONY_PSX".into(), "SONY_PS2".into()],
        )
        .await
        .unwrap();
    let code = r#"
        export async function onLaunch(args) {
            args.console = 'SONY_PS2';
            const entries = await args.firmwareStore.list();
            if (entries.length !== 1 || entries[0].platform !== 'SONY_PSX') throw Error('scope');
            let rejected = false;
            try { await args.firmwareStore.open('bios', 'Other'); } catch { rejected = true; }
            if (!rejected) throw Error('cross-platform read');
            const file = await args.firmwareStore.open(entries[0].type, entries[0].label);
            if (file.name !== 'BIOS' || await file.text() !== 'abc') throw Error('read');
            if (await file.digest('sha2') !== 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad') throw Error('digest');
            rejected = false;
            try { args.filesystem.mountFile('/bad', file, { mode: 'rw' }); } catch { rejected = true; }
            if (!rejected) throw Error('writable firmware');
            args.filesystem.mountFile('/bios', file, { mode: 'r' });
            args.filesystem.mountFile('/emulator.exe', new Uint8Array([1]), { mode: 'rx' });
            return { command: args.filesystem.path('/emulator.exe'), hooks: { graphics: ['vulkan'], input: [] }, surfaces: [{ index: 0 }] };
        }
    "#;
    fixture.plugin("emulator", "SONY_PSX", &code, true);
    let runtime = fixture.runtime();
    let result = runtime
        .handle()
        .invoke_with_storage_owned(
            "emulator",
            &["onLaunch".into()],
            vec![json!({
                "launchPath": fixture.input(), "console": "SONY_PSX", "package": {},
                "controllerTopology": { "mappings": [] }, "virtualize": true,
            })],
            BTreeMap::new(),
        )
        .await
        .unwrap();
    store
        .install(
            &[FirmwareInstallDescriptor {
                platform: "SONY_PSX".into(),
                firmware_type: "bios".into(),
                label: "BIOS".into(),
                contents: b"new".as_slice().into(),
            }],
            &["SONY_PSX".into()],
        )
        .await
        .unwrap();
    let prepared = result.prepared_filesystem.unwrap();
    let filesystem = prepared.filesystem();
    filesystem.materialize().unwrap();
    let path = "/bios".parse().unwrap();
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
    let mut bytes = [0; 3];
    assert_eq!(file.read(0, &mut bytes).unwrap(), 3);
    assert_eq!(&bytes, b"abc");
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

#[tokio::test]
async fn bootstrap_receives_platform_scoped_file_entries() {
    let fixture = Fixture::new();
    fixture.plugin("wii", "NINTENDO_WII", r#"
        export async function onBootstrap(device, context) {
            const entries = await context.firmwareStore.list();
            if (entries.length !== 1 || entries[0].platform !== device.platform) throw Error('bootstrap scope');
            const file = await context.firmwareStore.open(entries[0].type, entries[0].label);
            if (await file.text() !== 'secret') throw Error('bootstrap read');
        }
        export async function outside() { return __polymerFirmware.store.list(); }
    "#, false);
    let store = FirmwareStore::open(fixture.directories.firmware())
        .await
        .unwrap();
    store
        .install(
            &[
                FirmwareInstallDescriptor {
                    platform: "NINTENDO_WII".into(),
                    firmware_type: "key".into(),
                    label: "Key".into(),
                    contents: b"secret".as_slice().into(),
                },
                FirmwareInstallDescriptor {
                    platform: "SONY_PS2".into(),
                    firmware_type: "bios".into(),
                    label: "Other".into(),
                    contents: b"other".as_slice().into(),
                },
            ],
            &["NINTENDO_WII".into(), "SONY_PS2".into()],
        )
        .await
        .unwrap();
    let volume = StorageManager::new(fixture.directories.consolestorage())
        .create(StorageKind::Nand, "NINTENDO_WII", None)
        .await
        .unwrap();
    let device = Device {
        platform: "NINTENDO_WII".into(),
        id: volume.metadata().await.unwrap().id,
    };
    let runtime = fixture.runtime();
    runtime
        .handle()
        .bootstrap(device, json!({}), runtime.handle().cancellation_token())
        .await
        .unwrap();
    let error = runtime
        .handle()
        .invoke("wii", &["outside".into()], vec![])
        .await
        .unwrap_err();
    assert!(error.message.contains("host-bound"));
}
