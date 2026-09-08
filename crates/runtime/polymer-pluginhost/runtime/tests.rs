use super::*;
use crate::{
    actions::ParsedHotkey,
    controller::PauseState,
    scraper::{ScrapeCandidate, ScrapeScalar},
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};
use tempfile::TempDir;

struct TestRuntime {
    runtime: PluginRuntime,
    _storage: TempDir,
}

impl std::ops::Deref for TestRuntime {
    type Target = PluginRuntime;

    fn deref(&self) -> &Self::Target {
        &self.runtime
    }
}

fn test_runtime(registry: PluginRegistry, options: PluginRuntimeOptions) -> TestRuntime {
    let storage = tempfile::tempdir().unwrap();
    let directories = polymer_library::Directories::rooted(storage.path(), storage.path());
    TestRuntime {
        runtime: PluginRuntime::new(registry, directories, options),
        _storage: storage,
    }
}

fn package() -> (TempDir, PluginRegistry) {
    let root = tempfile::tempdir().unwrap();
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
    std::fs::create_dir(root.path().join("runtime")).unwrap();
    std::fs::write(root.path().join("runtime/emulator.exe"), b"emulator").unwrap();
    std::fs::write(root.path().join("game.iso"), b"game").unwrap();
    std::fs::write(
        root.path().join("config.toml"),
        r#"
            [targets.settings]
            output = { format = "ini", path = "settings.ini" }

            [sections.general]
            title = "General"
            target = "settings"

            [[sections.general.options]]
            key = "enabled"
            title = "Enabled"
            type = "bool"
            default = false
            emit = { key = "Enabled" }

            [actions]
            target = "settings"

            [actions.map]
            pause = { name = "TogglePause", expression = "F10" }

            [input]
            driver = "sdl3"
            device = "SDL-{index}"

            [input.mapping]
            [input.layouts]
            [platforms.TEST.ports]
        "#,
    )
    .unwrap();
    std::fs::write(
        root.path().join("plugin.js"),
        r#"
            import runtime from "pkg:runtime";
            import { uiAutomation } from "polymer:uiautomation";
            import { actions } from "polymer:actions";
            import { pluginStorage } from "polymer:pluginStorage";
            let isolateCounter = 0;
            export function onPluginLoad() {
              const loads = pluginStorage.getItem("loads") ?? 0;
              pluginStorage.setItem("loads", loads + 1);
              pluginStorage.setItem("cached", { names: ["graphics-pack"] });
            }
            export async function onConfigure(game, userConfiguration) {
              const dynamicRuntime = (await import("pkg:runtime")).default;
              if (dynamicRuntime !== runtime) throw new Error("package resource imports must share module identity");
              await import("polymer:api");
              for (const source of ["./plugin.js", "../plugin.js", "/plugin.js", "file:///plugin.js"]) {
                let rejected = false;
                try { await import(source); } catch { rejected = true; }
                if (!rejected) throw new Error(`path-based import was accepted: ${source}`);
              }
              let readOnly = false;
              try { await runtime.remove("emulator.exe"); } catch { readOnly = true; }
              if (!readOnly) throw new Error("package resources must remain read-only");
              const entries = [];
              for await (const entry of runtime) {
                entries.push(entry.name);
                if (entry.kind === "file") await entry.bytes();
              }
              return {
                schema: "worker/v1",
                groups: [{
                  key: "general",
                  title: entries.join(","),
                  options: [{ key: "enabled", source: "general.enabled" }],
                }],
                values: {
                  general: { enabled: userConfiguration?.general?.enabled === true },
                },
              };
            }
            export function named(value) {
              isolateCounter++;
              return { value, isolateCounter };
            }
            export function cached() {
              const value = pluginStorage.getItem("cached");
              value.names.push("isolate");
              return { loads: pluginStorage.getItem("loads"), value };
            }
            export async function asyncResult(value) {
              await Promise.resolve();
              return value * 2;
            }
            export function structured(value) {
              return [
                value.self === value,
                value.bigint + 1n,
                value.date.getTime(),
                value.map.get("key"),
                value.typed[1],
                value,
              ];
            }
            export function neverReturns() { while (true) {} }
            export function throwsError() {
              throw new Error("worker-test-boom");
            }
            export const notCallable = 17;
            export function onLaunch(args) {
              args.configuration.options().general.enabled = true;
              let command = "emulator.exe";
              let argv = [];
              if (args.virtualize) {
                const firstRender = args.configuration.render();
                const secondRender = args.configuration.render();
                if (firstRender === secondRender || firstRender["settings.ini"] === secondRender["settings.ini"]) {
                  throw new Error("configuration.render() did not return fresh values");
                }
                args.filesystem.mountTree("/Emulator", runtime, { mode: "rx" });
                for (const [name, bytes] of Object.entries(firstRender)) {
                  args.filesystem.mountFile(`/Emulator/Config/${name}`, bytes, { mode: "r" });
                }
                args.filesystem.mountHostFile("/Game/game.iso", args.launchPath, { mode: "r" });
                args.filesystem.mountScratch("/User", { mode: "rwx", backing: "memory" });
                args.filesystem.setWorkingDirectory("/Emulator");
                args.filesystem.setProfile({ home: "/User", temp: "/User" });
                command = args.filesystem.path("/Emulator/emulator.exe");
                argv.push(args.filesystem.path("/Game/game.iso"));
              }
              return {
                command,
                argv,
                hooks: { graphics: ["vulkan"], input: ["sdl3"] },
                surfaces: [{ index: 0, }],
              };
            }
            export async function onControllerChanged(context) {
              if (typeof actions.pause?.activate !== "function") {
                throw new Error("onControllerChanged did not receive configured actions");
              }
              const window = await uiAutomation.waitForWindow({ pattern: "Emulator" });
              await window.locator('button[name="Controllers"]').press();
              context.configuration.options().general.enabled = false;
              for (const [name, bytes] of Object.entries(context.configuration.render())) {
                context.filesystem.mountFile(`/Emulator/Config/${name}`, bytes, { mode: "r" });
              }
            }
            export function onParseHotkey(expression) {
              return { kind: "keyboard", code: expression, modifiers: ["Control"] };
            }
            export function onPauseResume(context) {
              if (context.pauseState !== "pause" && context.pauseState !== "resume") {
                throw new Error(`invalid pause state: ${context.pauseState}`);
              }
              actions.pause.activate();
            }
            export default {
              fallback(value) { return `fallback:${value}`; },
              nonFunction: 23,
              onLaunch,
              onControllerChanged,
              onParseHotkey,
              onPauseResume,
            };
        "#,
    )
    .unwrap();
    let registry = PluginRegistry::new();
    registry.load(root.path()).unwrap();
    (root, registry)
}

fn configuration_mount_package() -> (TempDir, PluginRegistry) {
    let (root, _) = package();
    std::fs::write(
        root.path().join("plugin.js"),
        r#"
            import runtime from "pkg:runtime";
            export function onParseHotkey(expression) {
              return { kind: "keyboard", code: expression };
            }
            export function onLaunch(args) {
              args.filesystem.mountTree("/Emulator", runtime, { mode: "rx" });
              args.filesystem.mountConfiguration("/Emulator/Config", args.configuration, { mode: "r" });
              return {
                command: args.filesystem.path("/Emulator/emulator.exe"),
                argv: [],
                hooks: { graphics: [], input: ["sdl3"] },
                surfaces: [{ index: 0, }],
              };
            }
        "#,
    )
    .unwrap();
    let registry = PluginRegistry::new();
    registry.load(root.path()).unwrap();
    (root, registry)
}

fn scraper_package(source: &str) -> (TempDir, PluginRegistry) {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("plugin.toml"),
        r#"
            [plugin]
            id = "scraper-test"
            version = "1"
            class = "scraper"
            main = "scraper.js"

            [scraper]
            requires = ["package.platform", "any(meta.title, filename.nointro.title)"]
            provides = ["meta.genre", "media.boxart"]
        "#,
    )
    .unwrap();
    std::fs::write(root.path().join("scraper.js"), source).unwrap();
    let registry = PluginRegistry::new();
    registry.load(root.path()).unwrap();
    (root, registry)
}

fn capability_package() -> (TempDir, PluginRegistry, String) {
    let root = tempfile::tempdir().unwrap();
    let id = format!("capability-test-{}", std::process::id());
    std::fs::write(
        root.path().join("plugin.toml"),
        format!(
            r#"
              [plugin]
              id = "{id}"
              version = "1"
              class = "scraper"
              main = "fixture.js"

              [[plugin.caches]]
              key = "fixture"
              label = "Fixture"

              [scraper]
              provides = []
            "#
        ),
    )
    .unwrap();
    std::fs::write(
        root.path().join("fixture.js"),
        r#"
          import { caches } from "polymer:cache";
          import { connect } from "polymer:database";
          export async function exercise() {
            __polymerFilesystem.filesystem.mountTree("/Cache", caches.fixture, { mode: "rwx" });
            const file = caches.fixture.open("nested/value.txt", { write: true, truncate: true });
            const writer = file.writable.getWriter();
            await writer.write("streamed-");
            await writer.write("cache");
            await writer.close();
            const cached = caches.fixture.open("nested/value.txt");
            const range = new TextDecoder().decode(await cached.read(9, 5));
            const db = await connect();
            await db.exec("CREATE TABLE IF NOT EXISTS values_table (value TEXT NOT NULL); DELETE FROM values_table;");
            await db.batch([
              ["INSERT INTO values_table VALUES (?)", ["one"]],
              { sql: "INSERT INTO values_table VALUES (?)", args: ["two"] },
            ]);
            await db.transactionAsync(async database => {
              await database.prepare("INSERT INTO values_table VALUES (?)").run(["three"]);
            });
            const values = [];
            for await (const row of db.prepare("SELECT value FROM values_table ORDER BY rowid").iterate()) values.push(row.value);
            await db.close();
            return { text: await cached.text(), range, values };
          }
        "#,
    )
    .unwrap();
    let registry = PluginRegistry::new();
    registry.load(root.path()).unwrap();
    (root, registry, id)
}

fn scrape_input() -> ScrapeInput {
    BTreeMap::from([
        (
            "package.platform".to_owned(),
            vec![ScrapeCandidate {
                source: "Gumshoe".to_owned(),
                value: ScrapeScalar::String("NINTENDO_NES".to_owned()),
            }],
        ),
        (
            "filename.nointro.title".to_owned(),
            vec![ScrapeCandidate {
                source: "Shiratsu No-Intro".to_owned(),
                value: ScrapeScalar::String("Metroid".to_owned()),
            }],
        ),
    ])
}

#[tokio::test(flavor = "current_thread")]
async fn scraper_yields_scalar_objects_incrementally_and_preserves_order() {
    let (_root, registry) = scraper_package(
        r#"
          export async function* onScrape(input) {
            if (input["filename.nointro.title"][0].value !== "Metroid") throw new Error("input");
            yield { key: "meta.genre", value: "Action", source: "Fixture" };
            await Promise.resolve();
            yield { key: "meta.genre", value: "Adventure", source: "Fixture" };
          }
        "#,
    );
    let runtime = test_runtime(registry, PluginRuntimeOptions::default());
    let handle = runtime.handle();
    let values = handle
        .scrape("scraper-test", scrape_input(), handle.cancellation_token())
        .await
        .unwrap();
    assert_eq!(
        values.iter().map(|value| &value.value).collect::<Vec<_>>(),
        [
            &ScrapeScalar::String("Action".to_owned()),
            &ScrapeScalar::String("Adventure".to_owned())
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn plugin_load_progress_is_visible_while_scraping_waits_for_initialization() {
    let (_root, registry) = scraper_package(
        r#"
        export async function onPluginLoad({ progress }) {
            progress({ message: "fixture import in progress", completed: 25, total: 100 });
            await new Promise(resolve => setTimeout(resolve, 300));
        }
        export async function* onScrape() {
            yield { key: "meta.genre", value: "Action", source: "Fixture" };
        }
    "#,
    );
    let runtime = test_runtime(registry, PluginRuntimeOptions::default());
    let handle = runtime.handle();
    let mut events = PluginEvent::subscribe();
    let pending = tokio::spawn(async move {
        handle
            .scrape("scraper-test", scrape_input(), handle.cancellation_token())
            .await
    });
    let progress = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = events.recv().await.unwrap();
            if event.detail["message"] == "fixture import in progress" {
                break event;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(progress.detail["state"], "loading");
    assert_eq!(progress.detail["completed"], 25);
    assert_eq!(progress.detail["total"], 100);
    assert!(!pending.is_finished());
    assert_eq!(pending.await.unwrap().unwrap().len(), 1);
    let terminal = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = events.recv().await.unwrap();
            if event.source_id == progress.source_id && event.detail["state"] == "ready" {
                break event;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(terminal.plugin_id, "scraper-test");
}

#[tokio::test(flavor = "current_thread")]
async fn launchbox_batches_run_on_the_plugin_database_during_load() {
    let (root, registry) = scraper_package(
        r#"
        import { connect } from "polymer:database";
        import { IndexWriter } from "pkg:index-writer.ts";
        export async function onPluginLoad({ progress }) {
            const db = await connect();
            await db.exec("CREATE TABLE items(id INTEGER PRIMARY KEY, name TEXT); BEGIN IMMEDIATE;");
            const writer = new IndexWriter(db, "INSERT INTO items", "fixture items");
            for (let i = 0; i < 1000; i++) writer.add([i, `Item ${i}`]);
            await writer.flush();
            await db.exec("COMMIT;");
            progress({ message: "fixture batches committed", completed: 1000, total: 1000 });
        }
        export async function* onScrape() {
            const db = await connect();
            const row = await db.prepare("SELECT COUNT(*) n FROM items").get();
            if (row.n !== 1000) throw new Error("Incomplete index");
            yield { key: "meta.genre", value: "Indexed", source: "Fixture" };
        }
    "#,
    );
    std::fs::write(
        root.path().join("index-writer.ts"),
        include_str!("../../../../plugins/launchbox/index-writer.ts"),
    )
    .unwrap();
    let runtime = test_runtime(registry, PluginRuntimeOptions::default());
    let handle = runtime.handle();
    let results = handle
        .scrape("scraper-test", scrape_input(), handle.cancellation_token())
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn failed_plugin_load_reports_failure_instead_of_leaving_a_progress_bar_running() {
    let (_root, registry) = scraper_package(
        r#"
        export async function onPluginLoad({ progress }) {
            progress({ message: "failure fixture importing", completed: 1, total: 2 });
            throw new Error("failure fixture import rejected");
        }
        export async function* onScrape() { throw new Error("must not scrape"); }
    "#,
    );
    let runtime = test_runtime(registry, PluginRuntimeOptions::default());
    let handle = runtime.handle();
    assert!(
        handle
            .scrape("scraper-test", scrape_input(), handle.cancellation_token())
            .await
            .is_err()
    );
    let mut events = PluginEvent::subscribe();
    assert!(std::iter::from_fn(|| events.try_recv().ok()).any(|event| {
        event.detail["state"] == "failed"
            && event.detail["message"]
                .as_str()
                .is_some_and(|message| message.contains("failure fixture import rejected"))
    }));
}

#[tokio::test(flavor = "current_thread")]
async fn scraper_rejects_non_generators_arrays_and_undeclared_keys() {
    for source in [
        "export function onScrape() { return [{ key: 'meta.genre', value: 'Action', source: 'Fixture' }]; }",
        "export async function* onScrape() { yield ['meta.genre', 'Action', 'Fixture']; }",
        "export async function* onScrape() { yield { key: 'meta.title', value: 'Metroid', source: 'Fixture' }; }",
        "export async function* onScrape() { yield { key: 'meta.genre', value: ['Action'], source: 'Fixture' }; }",
    ] {
        let (_root, registry) = scraper_package(source);
        let runtime = test_runtime(registry, PluginRuntimeOptions::default());
        let handle = runtime.handle();
        assert!(
            handle
                .scrape("scraper-test", scrape_input(), handle.cancellation_token())
                .await
                .is_err(),
            "invalid scraper source was accepted: {source}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn scraper_requires_every_clause_but_accepts_any_alternative() {
    let (_root, registry) = scraper_package("export async function* onScrape() {}");
    let plugin = registry
        .plugin::<dyn ScraperPluginInstance>("scraper-test")
        .unwrap();
    assert!(plugin.accepts(&scrape_input()));
    assert!(!plugin.accepts(&BTreeMap::from([(
        "package.platform".to_owned(),
        vec![ScrapeCandidate {
            source: "Gumshoe".to_owned(),
            value: ScrapeScalar::String("NINTENDO_NES".to_owned()),
        }],
    )])));
}

#[tokio::test(flavor = "current_thread")]
async fn scripted_plugins_receive_streaming_cache_and_injected_turso_capabilities() {
    let (_root, registry, id) = capability_package();
    let storage = tempfile::tempdir().unwrap();
    let directories = polymer_library::Directories::rooted(storage.path(), storage.path());
    let cache_file = directories
        .plugin_cache(&id)
        .join("fixture/nested/value.txt");
    let database = directories.plugin_database(&id);
    let runtime = PluginRuntime::new(registry, directories, PluginRuntimeOptions::default());
    let result = runtime
        .handle()
        .invoke(&id, &["exercise".to_owned()], Vec::new())
        .await
        .unwrap();
    assert_eq!(
        result,
        serde_json::json!({
            "text": "streamed-cache",
            "range": "cache",
            "values": ["one", "two", "three"],
        })
    );
    assert_eq!(
        std::fs::read_to_string(cache_file).unwrap(),
        "streamed-cache"
    );
    assert!(database.is_file());
}

struct RecordingAutomation(Arc<StdMutex<Vec<&'static str>>>);

impl polymer_automation::AutomationDriver for RecordingAutomation {
    fn execute(
        &mut self,
        request: polymer_automation::AutomationRequest,
    ) -> Result<polymer_automation::AutomationResponse, polymer_automation::AutomationError> {
        use polymer_automation::{AutomationRequest, AutomationResponse};
        match request {
            AutomationRequest::WaitForWindow { .. } => {
                self.0.lock().unwrap().push("window");
                Ok(AutomationResponse::Window(
                    polymer_automation::WindowSnapshot {
                        id: polymer_automation::WindowId("test-window".to_owned()),
                        process_id: 7,
                        title: "Emulator".to_owned(),
                        class_name: "TestWindow".to_owned(),
                        visible: true,
                    },
                ))
            }
            AutomationRequest::LocatorAction { .. } => {
                self.0.lock().unwrap().push("action");
                Ok(AutomationResponse::Unit)
            }
            _ => Err(polymer_automation::AutomationError::new(
                "unexpectedRequest",
                "test received an unexpected automation request",
            )),
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn facade_on_emulator_launch_materializes_its_invocation_configuration() {
    let (_root, registry) = package();
    let runtime = test_runtime(registry, PluginRuntimeOptions::default());
    let result = runtime
        .handle()
        .invoke(
            "worker-test",
            &["onLaunch".to_owned()],
            vec![serde_json::json!({
                "launchPath": "game",
                "console": "TEST",
                "controllerTopology": { "mappings": [] },
            })],
        )
        .await
        .unwrap();
    assert_eq!(result["command"], "emulator.exe");

    let explicit_default = runtime
        .handle()
        .invoke(
            "worker-test",
            &["default".to_owned(), "onLaunch".to_owned()],
            vec![serde_json::json!({
                "launchPath": "game",
                "console": "TEST",
                "controllerTopology": { "mappings": [] },
            })],
        )
        .await
        .unwrap();
    assert_eq!(explicit_default["command"], "emulator.exe");
}

#[tokio::test(flavor = "current_thread")]
async fn plugin_storage_is_shared_between_fresh_isolates() {
    let (_root, registry) = package();
    let runtime = test_runtime(registry, PluginRuntimeOptions::default());
    let handle = runtime.handle();
    let path = ["cached".to_owned()];

    for _ in 0..2 {
        assert_eq!(
            handle
                .invoke("worker-test", &path, Vec::new())
                .await
                .unwrap(),
            serde_json::json!({
                "loads": 1,
                "value": { "names": ["graphics-pack", "isolate"] },
            })
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn prepared_filesystems_are_returned_to_the_application_owner() {
    let (root, registry) = package();
    let runtime = test_runtime(registry, PluginRuntimeOptions::default());
    let handle = runtime.handle();
    let result = handle
        .invoke_with_storage_owned(
            "worker-test",
            &["onLaunch".to_owned()],
            vec![serde_json::json!({
                "launchPath": root.path().join("game.iso"),
                "console": "TEST",
                "controllerTopology": { "mappings": [] },
                "virtualize": true,
            })],
            BTreeMap::new(),
        )
        .await
        .unwrap();
    assert!(result.value["preparedFilesystem"].is_null());
    let filesystem = result.prepared_filesystem.unwrap();
    assert_eq!(filesystem.filesystem().compiled().bindings().len(), 4);
}

#[tokio::test(flavor = "current_thread")]
async fn controller_callback_regenerates_a_live_mounted_configuration() {
    let (root, registry) = package();
    let runtime = test_runtime(registry, PluginRuntimeOptions::default());
    let result = runtime
        .handle()
        .invoke_with_storage_owned(
            "worker-test",
            &["onLaunch".to_owned()],
            vec![serde_json::json!({
                "launchPath": root.path().join("game.iso"),
                "console": "TEST",
                "controllerTopology": { "mappings": [] },
                "virtualize": true,
            })],
            BTreeMap::new(),
        )
        .await
        .unwrap();
    let filesystem = result.prepared_filesystem.unwrap();
    let session = result.controller_session.unwrap();
    filesystem.filesystem().materialize().unwrap();
    let automation_calls = Arc::new(StdMutex::new(Vec::new()));
    let automation =
        polymer_automation::AutomationSession::spawn(RecordingAutomation(automation_calls.clone()))
            .unwrap();
    session.attach_automation(automation).unwrap();
    session
        .change(polymer_controllers::ControllerTopologyChange {
            previous: polymer_controllers::ControllerTopology::default(),
            requested: polymer_controllers::ControllerTopology::default(),
        })
        .unwrap()
        .await
        .unwrap()
        .unwrap();

    let path = "/Emulator/Config/settings.ini".parse().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    let rendered = loop {
        let file = filesystem
            .filesystem()
            .open(
                &path,
                polymer_vfs::OpenOptions {
                    read: true,
                    write: false,
                    truncate: false,
                },
            )
            .unwrap();
        let mut bytes = [0; 128];
        let length = file.read(0, &mut bytes).unwrap();
        let rendered = String::from_utf8_lossy(&bytes[..length]).into_owned();
        if rendered.contains("Enabled=false") || std::time::Instant::now() >= deadline {
            break rendered;
        }
        std::thread::yield_now();
    };
    assert!(rendered.contains("Enabled=false"), "{rendered}");
    assert_eq!(*automation_calls.lock().unwrap(), ["window", "action"]);
}

#[tokio::test(flavor = "current_thread")]
async fn pause_callback_activates_its_parsed_hotkey_action() {
    let (root, registry) = package();
    let runtime = test_runtime(registry, PluginRuntimeOptions::default());
    let result = runtime
        .handle()
        .invoke_with_storage_owned(
            "worker-test",
            &["onLaunch".to_owned()],
            vec![serde_json::json!({
                "launchPath": root.path().join("game.iso"),
                "console": "TEST",
                "controllerTopology": { "mappings": [] },
            })],
            BTreeMap::new(),
        )
        .await
        .unwrap();
    let session = result.controller_session.unwrap();
    let activated = Arc::new(StdMutex::new(Vec::new()));
    let recorded = activated.clone();
    session
        .attach_actions(move |hotkey| {
            recorded.lock().unwrap().push(hotkey.clone());
            Ok(())
        })
        .unwrap();
    session
        .pause_resume(PauseState::Pause, None)
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    session
        .pause_resume(PauseState::Resume, None)
        .unwrap()
        .await
        .unwrap()
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while activated.lock().unwrap().len() != 2 && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert_eq!(
        *activated.lock().unwrap(),
        [
            ParsedHotkey::Keyboard {
                code: "F10".to_owned(),
                modifiers: vec!["Control".to_owned()],
            },
            ParsedHotkey::Keyboard {
                code: "F10".to_owned(),
                modifiers: vec!["Control".to_owned()],
            },
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn configuration_mount_retains_a_controller_session_without_a_callback() {
    let (root, registry) = configuration_mount_package();
    let runtime = test_runtime(registry, PluginRuntimeOptions::default());
    let result = runtime
        .handle()
        .invoke_with_storage_owned(
            "worker-test",
            &["onLaunch".to_owned()],
            vec![serde_json::json!({
                "launchPath": root.path().join("game.iso"),
                "console": "TEST",
                "controllerTopology": { "mappings": [] },
            })],
            BTreeMap::new(),
        )
        .await
        .unwrap();
    let filesystem = result.prepared_filesystem.unwrap();
    let session = result.controller_session.unwrap();
    filesystem.filesystem().materialize().unwrap();
    session
        .change(polymer_controllers::ControllerTopologyChange {
            previous: polymer_controllers::ControllerTopology::default(),
            requested: polymer_controllers::ControllerTopology::default(),
        })
        .unwrap()
        .await
        .unwrap()
        .unwrap();

    let path = "/Emulator/Config/settings.ini".parse().unwrap();
    let file = filesystem
        .filesystem()
        .open(
            &path,
            polymer_vfs::OpenOptions {
                read: true,
                write: false,
                truncate: false,
            },
        )
        .unwrap();
    let mut bytes = [0; 128];
    let length = file.read(0, &mut bytes).unwrap();
    assert!(String::from_utf8_lossy(&bytes[..length]).contains("Enabled=false"));
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_terminates_executing_v8_code() {
    let (_root, registry) = package();
    let runtime = test_runtime(registry, PluginRuntimeOptions::default());
    let handle = runtime.handle();
    let cancellation = handle.clone();
    let path = ["neverReturns".to_owned()];
    let call = handle.invoke("worker-test", &path, vec![]);
    tokio::pin!(call);
    tokio::select! {
        result = &mut call => panic!("worker finished before cancellation: {result:?}"),
        () = tokio::time::sleep(Duration::from_millis(25)) => cancellation.cancel_all(),
    }
    let error = tokio::time::timeout(Duration::from_secs(2), call)
        .await
        .expect("termination did not stop V8")
        .unwrap_err();
    assert_eq!(error.code, "PLUGIN_CANCELLED");
}

#[tokio::test(flavor = "current_thread")]
async fn invocation_cancellation_does_not_abort_an_unrelated_call() {
    let (_root, registry) = package();
    let runtime = test_runtime(
        registry,
        PluginRuntimeOptions {
            max_concurrent_calls: 1,
        },
    );
    let handle = runtime.handle();
    let first_token = handle.cancellation_token();
    let first_cancel = first_token.clone();
    let first_handle = handle.clone();
    let first = tokio::spawn(async move {
        first_handle
            .invoke_with_cancellation(
                "worker-test",
                &["neverReturns".to_owned()],
                Vec::new(),
                first_token,
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(25)).await;

    let second_handle = handle.clone();
    let second = tokio::spawn(async move {
        second_handle
            .invoke(
                "worker-test",
                &["asyncResult".to_owned()],
                vec![serde_json::json!(9)],
            )
            .await
    });
    first_cancel.cancel();

    assert!(first.await.unwrap().is_err());
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), second)
            .await
            .expect("the unrelated call stayed queued")
            .unwrap()
            .unwrap(),
        serde_json::json!(18)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn update_contents_receives_progress_and_publishes_terminal_events() {
    for (path, fails) in [
        (vec!["updateContents".to_owned()], false),
        (
            vec!["default".to_owned(), "updateContents".to_owned()],
            true,
        ),
    ] {
        let source = format!(
            r#"
            export default {{
                async updateContents({{ progress }}) {{
                    progress({{ message: "refresh fixture progress", completed: 2, total: 4 }});
                    if ({fails}) throw new Error("refresh fixture failure");
                }}
            }};
            export async function* onScrape() {{}}
        "#
        );
        let (_root, registry) = scraper_package(&source);
        let runtime = test_runtime(registry, PluginRuntimeOptions::default());
        let mut events = PluginEvent::subscribe();
        while events.try_recv().is_ok() {}
        let result = runtime
            .handle()
            .invoke("scraper-test", &path, Vec::new())
            .await;
        assert_eq!(result.is_err(), fails);
        let events = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
        let progress = events
            .iter()
            .find(|event| event.detail["message"] == "refresh fixture progress")
            .unwrap();
        assert_eq!(progress.detail["completed"], 2);
        assert_eq!(progress.detail["total"], 4);
        let terminal = events
            .iter()
            .rev()
            .find(|event| event.source_id == progress.source_id)
            .unwrap();
        assert_eq!(
            terminal.detail["state"],
            if fails { "failed" } else { "ready" }
        );
    }
}
