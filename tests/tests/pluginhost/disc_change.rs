use polymer_library::LaunchableUri;
use polymer_pluginhost::{
    actions::ParsedHotkey,
    controller::{AttachActions, DiscChange, PauseResume, PauseState, Quit, StateAction},
    loading::PluginRegistry,
    runtime::{PluginRuntime, PluginRuntimeOptions},
};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

#[tokio::test(flavor = "current_thread")]
async fn disc_changes_keep_quit_on_the_last_successful_disc() {
    for export in [
        vec!["onLaunch".to_owned()],
        vec!["default".to_owned(), "onLaunch".to_owned()],
    ] {
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
        std::fs::write(
            root.path().join("config.toml"),
            r#"
            [targets.settings]
            output = { format = "ini", path = "settings.ini" }
            [actions]
            target = "settings"
            [actions.map]
            pause = { expression = "F1", name = "Pause" }
            save = { expression = "F2", name = "Save" }
            load = { expression = "F3", name = "Load" }
            disc = { expression = "F4", name = "Disc" }
            quit = { expression = "F5", name = "Quit" }
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
            let current = 1;
            let busy = false;
            let mounts = 0;
            let paused = false;
            let filesystem;
            const plugin = {
              onLaunch(args) {
                if (args.initialDiscIndex !== 1 || args.launchPath !== "disc1") throw Error("wrong initial disc");
                if (!Object.isFrozen(args.launchSet) || !Object.isFrozen(args.launchSet[0])) throw Error("mutable set");
                filesystem = args.filesystem;
                filesystem.mount("/Config/", {
                  ...args.configuration,
                  render() {
                    return { ...args.configuration.render(), "paused.txt": new TextEncoder().encode(String(paused)) };
                  },
                }, { mode: "r" });
                const mount = args.filesystem.mount;
                if (!mount("/Initial", args.package, { mode: "rx" }).endsWith("disc1.iso")) throw Error("wrong initial mount");
                args.filesystem.mount("/Emulator/emulator.exe", new Uint8Array([1]), { mode: "rx" });
                return { command: args.filesystem.path("/Emulator/emulator.exe"), hooks: { graphics: ["vulkan"], input: [] }, surfaces: [{ index: 0 }] };
              },
              onParseHotkey(code) { return { kind: "keyboard", code }; },
              onPauseResume(context) {
                paused = true;
                filesystem.flush();
                context.actions.pause.activate();
              },
              onStateSave(context) { context.actions.save.activate(); },
              onStateResume(context) { context.actions.load.activate(); },
              async onDiscChange(context) {
                if (this !== plugin || busy) throw Error("callback receiver or serialization lost");
                if (context.index === 2) throw Error("disc unavailable");
                if (!context.filesystem || context.launchSet[context.index].launchPath !== `disc${context.index}`) throw Error("wrong context");
                if (!context.filesystem.mount(`/Change${mounts++}`, context.launchSet[context.index].package, { mode: "rx" }).endsWith(`disc${context.index}.iso`)) throw Error("wrong target mount");
                busy = true;
                await new Promise(resolve => setTimeout(resolve, 5));
                context.actions.disc.activate();
                current = context.index;
                busy = false;
              },
              onQuit(context) {
                if (context.launchPath !== `disc${current}` || context.package.serial !== `${current}`) throw Error("quit disc mismatch");
                if (!context.filesystem.mount(`/Quit${mounts++}`, context.package, { mode: "rx" }).endsWith(`disc${current}.iso`)) throw Error("wrong active mount after disc change");
                context.actions.quit.activate();
                return null;
              },
            };
            export default plugin;
        "#).unwrap();
        let registry = PluginRegistry::new();
        registry.load(root.path()).unwrap();
        let storage = tempfile::tempdir().unwrap();
        let directories = polymer_library::Directories::rooted(storage.path(), storage.path());
        let runtime = PluginRuntime::new(registry, directories, PluginRuntimeOptions::default());
        let result = runtime
            .handle()
            .invoke_with_storage_owned(
                "worker-test",
                &export,
                vec![serde_json::json!({
                    "console": "TEST", "controllerTopology": { "mappings": [] },
                    "launchSet": [
                        { "launchPath": "disc0", "package": { "serial": "0", "uri": LaunchableUri::file(root.path().join("disc0.iso")).unwrap(), "bundle": { "entrypoint": "disc0.iso", "files": [{ "name": "disc0.iso", "text": "disc", "uri": null }] } } },
                        { "launchPath": "disc1", "package": { "serial": "1", "uri": LaunchableUri::file(root.path().join("disc1.iso")).unwrap(), "bundle": { "entrypoint": "disc1.iso", "files": [{ "name": "disc1.iso", "text": "disc", "uri": null }] } } },
                        { "launchPath": "disc2", "package": { "serial": "2", "uri": LaunchableUri::file(root.path().join("disc2.iso")).unwrap(), "bundle": { "entrypoint": "disc2.iso", "files": [{ "name": "disc2.iso", "text": "disc", "uri": null }] } } },
                    ],
                    "initialDiscIndex": 1,
                })],
                BTreeMap::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.value["canChangeDisc"], true);
        assert_eq!(result.value["initialDiscIndex"], 1);
        let prepared = result.prepared_filesystem.unwrap();
        let filesystem = prepared.filesystem();
        filesystem.materialize().unwrap();
        let path = "/Config/paused.txt".parse().unwrap();
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
        let mut bytes = [0; 5];
        let length = file.read(0, &mut bytes).unwrap();
        assert_eq!(&bytes[..length], b"false");
        drop(file);
        let session = result.controller_session.unwrap();
        let timeout = Duration::from_secs(2);
        let (sender, actions) = std::sync::mpsc::channel();
        session
            .send(AttachActions(Arc::new(move |hotkey| {
                sender
                    .send(hotkey.clone())
                    .map_err(|error| error.to_string())
            })))
            .await
            .unwrap()
            .unwrap();
        session
            .send(PauseResume {
                state: PauseState::Pause,
                timeout,
            })
            .await
            .unwrap()
            .unwrap();
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
        let length = file.read(0, &mut bytes).unwrap();
        assert_eq!(&bytes[..length], b"true");
        drop(file);
        for save in [true, false] {
            session
                .send(StateAction {
                    slot: 0,
                    save,
                    timeout,
                })
                .await
                .unwrap()
                .unwrap();
        }
        assert!(
            session
                .send(Quit { timeout })
                .await
                .unwrap()
                .unwrap()
                .is_none()
        );
        let error = session
            .send(DiscChange { index: 2, timeout })
            .await
            .unwrap()
            .unwrap_err();
        assert!(error.contains("disc unavailable"));
        session.send(Quit { timeout }).await.unwrap().unwrap();
        assert!(
            session
                .send(DiscChange { index: 3, timeout })
                .await
                .unwrap()
                .is_err()
        );
        let first = session.send(DiscChange { index: 0, timeout });
        let second = session.send(DiscChange { index: 1, timeout });
        let (first, second) = tokio::join!(first, second);
        first.unwrap().unwrap();
        second.unwrap().unwrap();
        session.send(Quit { timeout }).await.unwrap().unwrap();
        session.shutdown().await.unwrap();
        assert_eq!(
            actions.try_iter().collect::<Vec<_>>(),
            ["F1", "F2", "F3", "F5", "F5", "F4", "F4", "F5"].map(|code| ParsedHotkey::Keyboard {
                code: code.to_owned(),
                modifiers: Vec::new(),
            })
        );
    }
}
