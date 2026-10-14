use polymer_library::{Directories, StorageKind, StorageManager};
use polymer_pluginhost::{
    loading::PluginRegistry,
    runtime::{PluginRuntime, PluginRuntimeOptions},
};

use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

struct Fixture {
    root: tempfile::TempDir,
    registry: PluginRegistry,
    directories: Directories,
    manager: Arc<StorageManager>,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let directories = Directories::rooted(root.path(), root.path());
        Self {
            manager: Arc::new(StorageManager::new(directories.consolestorage())),
            directories,
            root,
            registry: PluginRegistry::new(),
        }
    }
    fn provider(&self, id: &str, code: &str) -> std::path::PathBuf {
        let path = self.root.path().join(id);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(
            path.join("plugin.toml"),
            format!(
                r#"
            [plugin]
            id = "{id}"
            version = "1"
            class = "platform"
            main = "main.js"
            [platform]
            platforms = ["NINTENDO_WII"]
        "#
            ),
        )
        .unwrap();
        std::fs::write(path.join("main.js"), code).unwrap();
        path
    }
    fn emulator(&self) {
        let path = self.root.path().join("emulator");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(
            path.join("plugin.toml"),
            r#"
            [plugin]
            id = "emulator"
            version = "1"
            class = "emulator"
            main = "main.js"
            [emulator]
            platforms = ["NINTENDO_WII"]
            [emulator.configuration]
            definitions = ["config.toml"]
        "#,
        )
        .unwrap();
        std::fs::write(
            path.join("config.toml"),
            r#"
            [targets.settings]
            output = { format = "ini", path = "settings.ini" }
            [input]
            driver = "sdl3"
            device = "SDL-{index}"
            [input.mapping]
            [input.layouts]
            [platforms.NINTENDO_WII.ports]
        "#,
        )
        .unwrap();
        std::fs::write(
            path.join("main.js"),
            "export function onLaunch() { throw new Error('ON_LAUNCH_REACHED'); }",
        )
        .unwrap();
        self.registry.load(path).unwrap();
    }
    fn runtime(&self) -> PluginRuntime {
        PluginRuntime::new(
            self.registry.clone(),
            self.directories.clone(),
            PluginRuntimeOptions::default(),
        )
    }
    async fn device(&self) -> polymer_library::VolumeInfo {
        let volume = self
            .manager
            .create(StorageKind::Nand, "NINTENDO_WII", None)
            .await
            .unwrap();
        volume.metadata().await.unwrap()
    }
}

#[tokio::test]
async fn duplicate_provider_and_direct_bootstrap_are_rejected() {
    let fixture = Fixture::new();
    fixture
        .registry
        .load(fixture.provider("first", "export function onBootstrap() {}"))
        .unwrap();
    let duplicate = fixture.registry.load(fixture.provider("second", ""));
    assert!(
        duplicate
            .err()
            .unwrap()
            .to_string()
            .contains("duplicate providers")
    );
    assert_eq!(
        fixture.registry.platform("NINTENDO_WII").unwrap().id(),
        "first"
    );
    let runtime = fixture.runtime();
    let error = runtime
        .handle()
        .invoke("first", &["onBootstrap".into()], vec![])
        .await
        .unwrap_err();
    assert_eq!(error.code, "BOOTSTRAP_REQUIRES_DEVICE");
}

#[tokio::test]
async fn cancelling_bootstrap_terminates_the_worker() {
    let fixture = Fixture::new();
    fixture
        .registry
        .load(fixture.provider(
            "platform",
            r#"
        import { pluginStorage } from 'polymer:pluginStorage';
        export async function onBootstrap() {
            pluginStorage.setItem('entered', true);
            await new Promise(resolve => setTimeout(resolve, 60000));
            return {issues: []};
        }
        export function entered() { return pluginStorage.getItem('entered') === true; }
    "#,
        ))
        .unwrap();
    let runtime = fixture.runtime();
    let device = fixture.device().await;
    let token = runtime.handle().cancellation_token();
    let handle = runtime.handle().clone();
    let cancellation = token.clone();
    let manager = fixture.manager.clone();
    let operation = tokio::spawn(async move {
        handle
            .bootstrap(
                manager,
                &device.platform,
                device.id,
                Value::Null,
                cancellation,
                None,
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(10), async {
        while runtime
            .handle()
            .invoke("platform", &["entered".into()], vec![])
            .await
            .unwrap()
            != true
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    token.cancel();
    let error = tokio::time::timeout(Duration::from_secs(5), operation)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, "PLUGIN_CANCELLED");
}

#[tokio::test]
async fn jobs_started_during_bootstrap_are_children_of_its_job() {
    use polymer_library::jobs::{JobKind, JobSpec, JobState, JobStore};
    let fixture = Fixture::new();
    fixture
        .registry
        .load(fixture.provider(
            "progress-platform",
            r#"

        import { Job } from "polymer:jobs";
        export async function onBootstrap(device, context) {
            await new Job('Preparing firmware').run(() => {});
            throw new Error('BOOTSTRAP_FAILURE');
        }
    "#,
        ))
        .unwrap();
    let device = fixture.device().await;
    let directory = tempfile::tempdir().unwrap();
    let jobs = JobStore::open(directory.path().join("jobs.db"))
        .await
        .unwrap();
    let runtime = PluginRuntime::new(
        fixture.registry.clone(),
        fixture.directories.clone(),
        PluginRuntimeOptions {
            jobs: Some(jobs.clone()),
            ..Default::default()
        },
    );
    let job = Arc::new(
        jobs.start(JobSpec {
            kind: JobKind::Bootstrap,
            parent: None,
            title: "Installing system files".to_owned(),
            subject: Default::default(),
            params: Value::Null,
            dedupe: None,
            steps: Vec::new(),
            persistent: true,
        })
        .await
        .unwrap(),
    );
    let error = runtime
        .handle()
        .bootstrap(
            fixture.manager.clone(),
            &device.platform,
            device.id,
            Value::Null,
            runtime.handle().cancellation_token(),
            Some(job.clone()),
        )
        .await
        .unwrap_err();
    assert!(error.message.contains("BOOTSTRAP_FAILURE"));
    job.finish(JobState::Failed, Some(error.message))
        .await
        .unwrap();
    let listed = jobs.list().await.unwrap();
    let child = listed
        .iter()
        .find(|job| job.title == "Preparing firmware")
        .unwrap();
    assert_eq!(child.parent, Some(job.id()));
    assert_eq!(child.state, JobState::Completed);
}

impl Fixture {
    fn title_source(&self, root: &std::path::Path) {
        for directory in ["code", "content", "meta"] {
            std::fs::create_dir_all(root.join(directory)).unwrap();
        }
        std::fs::write(
            root.join("code/app.xml"),
            "<app><title_id>0005000c12345678</title_id></app>",
        )
        .unwrap();
        std::fs::write(root.join("code/cos.xml"), "<app/>").unwrap();
        std::fs::write(
            root.join("meta/meta.xml"),
            "<menu><title_id>0005000c12345678</title_id><title_version>7</title_version></menu>",
        )
        .unwrap();
        std::fs::write(root.join("content/data.bin"), b"content").unwrap();
        let ticket = polymer_consolestorage::impls::wiiu::ticket::WiiUTicket::synthesize(
            0x0005000c12345678,
            7,
            &[0x42; 16],
        )
        .unwrap();
        std::fs::write(root.join("code/title.tik"), ticket).unwrap();
    }
}

#[tokio::test]
async fn platform_install_persists_keys_before_validation_and_installs_content() {
    use polymer_consolestorage::keys::{KeyIdentity, KeyReader};
    use polymer_library::keys::KeyStore;
    let fixture = Fixture::new();
    let path = fixture.root.path().join("title");
    fixture.title_source(&path);
    let manager = StorageManager::new(fixture.directories.consolestorage());
    let volume = manager
        .create(StorageKind::Nand, "NINTENDO_WIIU", None)
        .await
        .unwrap();
    let device = volume.metadata().await.unwrap();
    let nand = manager
        .open(StorageKind::Nand, &device.platform, device.id)
        .await
        .unwrap()
        .as_nand()
        .unwrap();
    nand.root()
        .create("unfinished")
        .unwrap()
        .write_all_at(0, b"dirty")
        .unwrap();
    let error = manager
        .install(&device.platform, device.id, &path, |_| {})
        .await
        .unwrap_err();
    assert!(error.contains("uncommitted"), "{error}");
    let store = KeyStore::open(fixture.directories.keys()).await.unwrap();
    let keys = store.for_platform("NINTENDO_WIIU").unwrap();
    let key = keys
        .read(&KeyIdentity {
            namespace: "content".into(),
            id: "0005000c12345678".into(),
        })
        .await
        .unwrap();
    assert_eq!(key.bytes, [0x42; 16]);
    nand.recover().unwrap();
    let installed = manager
        .install(&device.platform, device.id, &path, |_| {})
        .await
        .unwrap();
    assert_eq!(installed.id, "0005000c12345678");
    let composed = nand
        .compose(None, std::sync::Arc::new(keys.clone()))
        .await
        .unwrap();
    let root = "usr/title/0005000c/12345678";
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(
        &mut composed
            .open_read(&format!("{root}/code/title.tik"))
            .unwrap(),
        &mut bytes,
    )
    .unwrap();
    assert_eq!(
        polymer_consolestorage::impls::wiiu::ticket::WiiUTicket::decode(&bytes)
            .unwrap()
            .bytes,
        key.bytes
    );
    assert_eq!(&bytes[0x1e6..0x1e8], &7u16.to_be_bytes());
    assert!(composed.exists(&format!("{root}/content/data.bin")));
    assert!(!nand.is_dirty().unwrap());

    // A recognized source with a malformed ticket is a preparation error.
    std::fs::write(path.join("code/title.tik"), b"truncated").unwrap();
    assert!(nand.try_install(&path, &keys).is_err());
    std::fs::remove_file(path.join("code/title.tik")).unwrap();
    assert!(
        nand.try_install(&path, &keys)
            .unwrap()
            .embedded_keys()
            .is_empty()
    );
    nand.uninstall(&[installed.id.as_str()]).await.unwrap();
    manager
        .install(&device.platform, device.id, &path, |_| {})
        .await
        .unwrap();
}

#[tokio::test]
async fn bootstrap_uses_supplied_storage_and_native_requirements_with_one_worker() {
    use polymer_consolestorage::keys::{KeyIdentity, KeyReader};
    let fixture = Fixture::new();
    let package = fixture.provider(
        "kind-check",
        r#"
        export async function onBootstrap(device, {keyStore}) {
            await device.install('pkg:title');
            const key = await keyStore.open({namespace: 'content', id: '0005000c12345678'});
            if ((await key.bytes()).length !== 16) throw new Error('installed key unavailable');
        }
    "#,
    );
    let manifest = package.join("plugin.toml");
    std::fs::write(
        &manifest,
        std::fs::read_to_string(&manifest)
            .unwrap()
            .replace("NINTENDO_WII", "NINTENDO_WIIU"),
    )
    .unwrap();
    fixture.title_source(&package.join("title"));
    fixture.registry.load(package).unwrap();
    // Storage belongs to the caller, independently of the plugin directories.
    let manager = Arc::new(StorageManager::new(
        fixture.root.path().join("console-storage"),
    ));
    let volume = manager
        .create(StorageKind::Nand, "NINTENDO_WIIU", None)
        .await
        .unwrap();
    let device = volume.metadata().await.unwrap();
    let nand = manager
        .open(StorageKind::Nand, &device.platform, device.id)
        .await
        .unwrap()
        .as_nand()
        .unwrap();
    nand.root()
        .create("unfinished")
        .unwrap()
        .write_all_at(0, b"dirty")
        .unwrap();
    let runtime = PluginRuntime::new(
        fixture.registry.clone(),
        fixture.directories.clone(),
        PluginRuntimeOptions {
            max_concurrent_calls: 1,
            ..Default::default()
        },
    );
    let error = tokio::time::timeout(
        Duration::from_secs(10),
        runtime.handle().bootstrap(
            manager.clone(),
            &device.platform,
            device.id,
            Value::Null,
            runtime.handle().cancellation_token(),
            None,
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.message.contains("uncommitted"), "{error}");
    let store = polymer_library::keys::KeyStore::open(manager.root().join("keys"))
        .await
        .unwrap();
    let record = store
        .for_platform("NINTENDO_WIIU")
        .unwrap()
        .read(&KeyIdentity {
            namespace: "content".into(),
            id: "0005000c12345678".into(),
        })
        .await
        .unwrap();
    assert_eq!(record.bytes, [0x42; 16]);
    nand.recover().unwrap();
    let inspection = runtime
        .handle()
        .bootstrap(
            manager,
            &device.platform,
            device.id,
            Value::Null,
            runtime.handle().cancellation_token(),
            None,
        )
        .await
        .unwrap();
    assert!(!inspection.dirty);
    assert!(
        inspection
            .artifacts
            .iter()
            .any(|artifact| artifact.id == "0005000c12345678")
    );
    assert!(
        nand.root()
            .exists("usr/title/0005000c/12345678/content/data.bin")
    );
}

#[tokio::test]
async fn try_install_retains_depot_without_starting_extraction() {
    let fixture = Fixture::new();
    let path = fixture.root.path().join("title");
    fixture.title_source(&path);
    let manager = StorageManager::new(fixture.directories.consolestorage());
    let volume = manager
        .create(StorageKind::Nand, "NINTENDO_WIIU", None)
        .await
        .unwrap();
    let device = volume.metadata().await.unwrap();
    let nand = manager
        .open(StorageKind::Nand, &device.platform, device.id)
        .await
        .unwrap()
        .as_nand()
        .unwrap();
    let store = polymer_library::keys::KeyStore::open(fixture.directories.keys())
        .await
        .unwrap();
    let keys = store.for_platform("NINTENDO_WIIU").unwrap();
    let installation = nand.try_install(&path, &keys).unwrap();
    assert!(!nand.is_dirty().unwrap());
    assert!(nand.artifacts().await.unwrap().is_empty());
    let artifact = installation.finish().await.unwrap();
    assert_eq!(artifact.id(), "0005000c12345678");
}

#[tokio::test]
async fn rust_and_javascript_share_platform_keys_and_immutable_files() {
    use polymer_consolestorage::keys::{KeyIdentity, KeyQuery, KeyReader, KeyRecord, KeyWriter};
    use polymer_library::keys::KeyStore;
    let fixture = Fixture::new();
    let store = KeyStore::open(fixture.directories.keys()).await.unwrap();
    let keys = store.for_platform("NINTENDO_WII").unwrap();
    let identity = KeyIdentity {
        namespace: "content".into(),
        id: "license".into(),
    };
    keys.put(&[KeyRecord {
        identity: identity.clone(),
        title_id: Some("title".into()),
        bytes: vec![1, 2, 3],
    }])
    .await
    .unwrap();
    fixture.registry.load(fixture.provider("key-platform", r#"
        export async function onBootstrap(device, {keyStore}) {
            const entries = await keyStore.list({namespace: 'content', titleId: 'title'});
            if (entries.length !== 1 || entries[0].byteLength !== 3) throw new Error('bad enumeration');
            const opened = await keyStore.open(entries[0]);
            await keyStore.put([{namespace: 'content', id: 'license', bytes: new Uint8Array([4, 5])}]);
            if ((await keyStore.list({titleId: 'title'})).length !== 0) throw new Error('stale title association');
            if (String(await (await keyStore.open(entries[0])).bytes()) !== '4,5') throw new Error('replacement not visible');
            if (!await keyStore.remove(entries[0])) throw new Error('remove failed');
            if (String(await opened.bytes()) !== '1,2,3') throw new Error('open resource changed');
            await keyStore.put([{namespace: 'system', id: 'native_name', bytes: new Uint8Array([7])}]);
        }
    "#)).unwrap();
    let runtime = fixture.runtime();
    let device = fixture.device().await;
    runtime
        .handle()
        .bootstrap(
            fixture.manager.clone(),
            &device.platform,
            device.id,
            Value::Null,
            runtime.handle().cancellation_token(),
            None,
        )
        .await
        .unwrap();
    assert!(keys.read(&identity).await.is_err());
    assert_eq!(
        keys.read(&KeyIdentity {
            namespace: "system".into(),
            id: "native_name".into()
        })
        .await
        .unwrap()
        .bytes,
        [7]
    );
    assert!(
        store
            .for_platform("SONY_PS3")
            .unwrap()
            .list(&KeyQuery::default())
            .await
            .unwrap()
            .is_empty()
    );

    fixture.emulator();
    std::fs::write(fixture.root.path().join("emulator/main.js"), r#"
        export async function onLaunch({keyStore, filesystem}) {
            const entry = await keyStore.open({namespace: 'system', id: 'native_name'});
            if (String(await entry.bytes()) !== '7') throw new Error('Rust/JS read failed');
            let denied = false;
            try { await __polymerKeys.writer.put([{namespace: 'system', id: 'native_name', bytes: new Uint8Array([9])}]); }
            catch (error) { denied = String(error).includes('read-only'); }
            if (!denied) throw new Error('host did not enforce reader');
            denied = false;
            try { filesystem.mount('/key', entry, {mode: 'rw'}); }
            catch { denied = true; }
            if (!denied) throw new Error('key resource permitted writable mount');
            throw new Error('KEY_READER_VERIFIED');
        }
    "#).unwrap();
    let error = runtime
        .handle()
        .invoke(
            "emulator",
            &["onLaunch".into()],
            vec![json!({
                "console": "NINTENDO_WII", "controllerTopology": {"mappings": []},
                "launchSet": [{"package": {"uri": "file:///missing.iso", "filename": "missing.iso"}}],
            })],
        )
        .await
        .unwrap_err();
    assert!(error.message.contains("KEY_READER_VERIFIED"), "{error}");
}

#[tokio::test]
async fn plugin_jobs_are_queued_and_fail_when_left_unfinished() {
    use polymer_library::jobs::{JobState, JobStore};
    let fixture = Fixture::new();
    fixture
        .registry
        .load(fixture.provider(
            "jobs-platform",
            r#"
        import { Job } from "polymer:jobs";
        export async function work() {
            const done = new Job('Indexing');
            done.progress({message: 'Indexing entries', completed: 3, total: 3});
            await done.complete();
            new Job('Abandoned').progress({message: 'Working'});
        }
    "#,
        ))
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let jobs = JobStore::open(directory.path().join("jobs.db"))
        .await
        .unwrap();
    let runtime = PluginRuntime::new(
        fixture.registry.clone(),
        fixture.directories.clone(),
        PluginRuntimeOptions {
            jobs: Some(jobs.clone()),
            ..Default::default()
        },
    );
    runtime
        .handle()
        .invoke("jobs-platform", &["work".to_owned()], Vec::new())
        .await
        .unwrap();
    let listed = jobs.list().await.unwrap();
    let done = listed.iter().find(|job| job.title == "Indexing").unwrap();
    assert_eq!(done.state, JobState::Completed);
    assert_eq!(done.message.as_deref(), Some("Indexing entries"));
    assert_eq!(done.subject.plugin_id.as_deref(), Some("jobs-platform"));
    let abandoned = listed.iter().find(|job| job.title == "Abandoned").unwrap();
    assert_eq!(abandoned.state, JobState::Failed);
}

#[tokio::test]
async fn jobs_started_in_a_run_scope_are_its_children_across_awaits() {
    use polymer_library::jobs::{JobState, JobStore};
    let fixture = Fixture::new();
    fixture
        .registry
        .load(fixture.provider(
            "nested-jobs-platform",
            r#"
        import { Job } from "polymer:jobs";
        const scope = (title) => new Job(title).run(async () => {
            await new Promise(resolve => setTimeout(resolve, 0));
            await new Job(`${title} child`).run(child => child.progress({message: `${title} work`}));
        });
        export async function work() {
            await Promise.all([scope('First'), scope('Second')]);
        }
    "#,
        ))
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let jobs = JobStore::open(directory.path().join("jobs.db"))
        .await
        .unwrap();
    let runtime = PluginRuntime::new(
        fixture.registry.clone(),
        fixture.directories.clone(),
        PluginRuntimeOptions {
            jobs: Some(jobs.clone()),
            ..Default::default()
        },
    );
    runtime
        .handle()
        .invoke("nested-jobs-platform", &["work".to_owned()], Vec::new())
        .await
        .unwrap();
    let listed = jobs.list().await.unwrap();
    for title in ["First", "Second"] {
        let parent = listed.iter().find(|job| job.title == title).unwrap();
        let child = listed
            .iter()
            .find(|job| job.title == format!("{title} child"))
            .unwrap();
        assert_eq!(parent.state, JobState::Completed);
        assert_eq!(child.parent, Some(parent.id));
        assert_eq!(
            child.message.as_deref(),
            Some(format!("{title} work").as_str())
        );
    }
}

#[tokio::test]
async fn tracked_streams_report_bytes_read_to_their_scope_and_enforce_a_limit() {
    use polymer_library::jobs::{JobState, JobStore};
    let fixture = Fixture::new();
    fixture
        .registry
        .load(fixture.provider(
            "tracked-platform",
            r#"
        import { Job, JobTrackedStream } from "polymer:jobs";
        const source = () => new ReadableStream({
            start(controller) {
                controller.enqueue(new Uint8Array(3));
                controller.enqueue(new Uint8Array(4));
                controller.close();
            },
        });
        export async function work() {
            await new Job('Reading').run(async () => {
                await new Response(new JobTrackedStream(source(), { message: 'Reading bytes', total: 7 })).arrayBuffer();
            });
            const limited = new JobTrackedStream(source(), { maxByteLength: 5 });
            return await new Response(limited).arrayBuffer().then(() => 'read', error => error.name);
        }
    "#,
        ))
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let jobs = JobStore::open(directory.path().join("jobs.db"))
        .await
        .unwrap();
    let runtime = PluginRuntime::new(
        fixture.registry.clone(),
        fixture.directories.clone(),
        PluginRuntimeOptions {
            jobs: Some(jobs.clone()),
            ..Default::default()
        },
    );
    let limited = runtime
        .handle()
        .invoke("tracked-platform", &["work".to_owned()], Vec::new())
        .await
        .unwrap();
    assert_eq!(limited, json!("RangeError"));
    let listed = jobs.list().await.unwrap();
    let reading = listed.iter().find(|job| job.title == "Reading").unwrap();
    assert_eq!(reading.state, JobState::Completed);
    assert_eq!(reading.message.as_deref(), Some("Reading bytes"));
    assert_eq!((reading.completed, reading.total), (Some(7), Some(7)));
}

#[tokio::test]
async fn platform_plugins_support_updates_by_exporting_both_functions() {
    let fixture = Fixture::new();
    fixture
        .registry
        .load(fixture.provider(
            "updating-platform",
            r#"
        export default {
            async *onCheckUpdates() {},
            async onFetchUpdate() {},
        };
    "#,
        ))
        .unwrap();
    let runtime = fixture.runtime();
    assert!(
        runtime
            .handle()
            .supports_updates("updating-platform")
            .await
            .unwrap()
    );

    let fixture = Fixture::new();
    fixture
        .registry
        .load(fixture.provider(
            "checking-platform",
            r#"
        export async function* onCheckUpdates() {}
    "#,
        ))
        .unwrap();
    let runtime = fixture.runtime();
    assert!(
        !runtime
            .handle()
            .supports_updates("checking-platform")
            .await
            .unwrap()
    );
}
