use blitz_dom::DocumentConfig;
use std::sync::{Arc, Mutex};

use super::{
    EmulatorCommand, EmulatorCommandSink, InputCommand, InputCommandSink, Mutation, Runtime,
    SnowglassNotification, WebRtcCommand, WebRtcCommandSink,
};

fn command_recorder() -> (EmulatorCommandSink, Arc<Mutex<Vec<EmulatorCommand>>>) {
    let commands = Arc::new(Mutex::new(Vec::new()));
    let recorded = commands.clone();
    (
        EmulatorCommandSink::new(move |command| recorded.lock().unwrap().push(command)),
        commands,
    )
}

fn input_command_recorder() -> (InputCommandSink, Arc<Mutex<Vec<InputCommand>>>) {
    let commands = Arc::new(Mutex::new(Vec::new()));
    let recorded = commands.clone();
    (
        InputCommandSink::new(move |command| recorded.lock().unwrap().push(command)),
        commands,
    )
}

fn drain_input_commands(commands: &Mutex<Vec<InputCommand>>) -> Vec<InputCommand> {
    std::mem::take(&mut *commands.lock().unwrap())
}

fn document(html: &str) -> blitz_html::HtmlDocument {
    let mut document = blitz_html::HtmlDocument::from_html(html, DocumentConfig::default());
    document
        .as_mut()
        .set_viewport(blitz_traits::shell::Viewport::new(
            800,
            600,
            1.0,
            blitz_traits::shell::ColorScheme::Dark,
        ));
    document.as_mut().resolve(1.0);
    document
}

#[tokio::test]
async fn node_dispatch_event_reaches_delegated_ancestors() {
    let html = r##"
        <html><body>
          <button id="drag-handle">drag</button>
          <script>
            document.addEventListener("dragstart", event => {
              event.preventDefault();
              postMessage({ type: "delegated", target: event.target.id });
            });
            const accepted = document.querySelector("#drag-handle").dispatchEvent(
              new CustomEvent("dragstart", { bubbles: true, cancelable: true })
            );
            postMessage({ type: "dispatch-result", accepted });
          </script>
        </body></html>
    "##;
    let document = document(html);
    let (command_sink, _) = command_recorder();
    let mut runtime = Runtime::new(
        document.as_ref(),
        html,
        "snowglass:///delegated-event-test.html",
        800,
        600,
        command_sink,
    )
    .await
    .unwrap();
    runtime.poll_event_loop().await.unwrap();

    assert_eq!(
        runtime.take_messages(),
        [
            serde_json::json!({ "type": "delegated", "target": "drag-handle" }),
            serde_json::json!({ "type": "dispatch-result", "accepted": false }),
        ]
    );
}

#[tokio::test]
async fn shell_input_api_sends_revisioned_transactions_and_resolves_results() {
    let html = r##"
        <html><body>
          <script>
            Snowglass.input.configure({
              operations: [{ type: "resetPortOverrides", port: "controller.0" }]
            }).then((configuration) => {
              postMessage({
                type: "input-configured",
                revision: configuration.revision
              });
            });
          </script>
        </body></html>
    "##;
    let document = document(html);
    let (command_sink, _) = command_recorder();
    let (input_sink, commands) = input_command_recorder();
    let plugin_runtime = Arc::new(snowglass_pluginhost::PluginRuntime::new(
        snowglass_pluginhost::PluginRegistry::new(),
        snowglass_pluginhost::PluginRuntimeOptions::default(),
    ));
    let (mut runtime, notifications) = Runtime::new_with_command_sinks_and_notifications(
        document.as_ref(),
        html,
        "snowglass:///input-test.html",
        800,
        600,
        command_sink,
        input_sink,
        WebRtcCommandSink::default(),
        plugin_runtime,
    )
    .await
    .unwrap();

    assert!(matches!(
        drain_input_commands(&commands).as_slice(),
        [InputCommand::Configure {
            request_id: 1,
            transaction,
        }] if transaction["expectedRevision"] == 0
            && transaction["operations"][0]["type"] == "resetPortOverrides"
            && transaction["operations"][0]["port"] == "controller.0"
    ));

    notifications
        .send(SnowglassNotification::InputConfigurationSettled {
            request_id: 1,
            result: serde_json::json!({
                "ok": true,
                "configuration": {
                    "revision": 1,
                    "ports": []
                }
            }),
        })
        .unwrap();
    runtime.poll_event_loop().await.unwrap();
    assert_eq!(
        runtime.take_messages(),
        [serde_json::json!({
            "type": "input-configured",
            "revision": 1,
        })]
    );
}

#[tokio::test]
async fn snowglass_is_the_only_public_native_namespace() {
    let html = r##"
        <html><body><script>
          postMessage({
            keys: Object.keys(Snowglass).sort(),
            frozen: Object.isFrozen(Snowglass),
            readonly: Object.getOwnPropertyDescriptor(globalThis, "Snowglass").writable === false,
            legacy: [typeof Emulator, typeof snowglassInput, typeof snowglassLauncher, typeof snowglass],
          });
        </script></body></html>
    "##;
    let document = document(html);
    let mut runtime = Runtime::new(
        document.as_ref(),
        html,
        "snowglass:///namespace-test.html",
        800,
        600,
        EmulatorCommandSink::default(),
    )
    .await
    .unwrap();
    runtime.poll_event_loop().await.unwrap();
    assert_eq!(
        runtime.take_messages(),
        [serde_json::json!({
            "keys": ["configuration", "emulator", "input", "plugin", "scraping", "webrtc"],
            "frozen": true,
            "readonly": true,
            "legacy": ["undefined", "undefined", "undefined", "undefined"],
        })]
    );
}

#[tokio::test]
async fn location_reflects_the_shell_document_url() {
    let html = r#"
        <html><body><script>
          postMessage({
            same: window.location === location,
            constructor: location instanceof Location,
            href: location.href,
            origin: location.origin,
            pathname: location.pathname,
            search: location.search,
            hash: location.hash,
          });
        </script></body></html>
    "#;
    let document = document(html);
    let mut runtime = Runtime::new(
        document.as_ref(),
        html,
        "https://example.invalid/app/index.html?mode=test#shell",
        800,
        600,
        EmulatorCommandSink::default(),
    )
    .await
    .unwrap();
    runtime.poll_event_loop().await.unwrap();
    assert_eq!(
        runtime.take_messages(),
        [serde_json::json!({
            "same": true,
            "constructor": true,
            "href": "https://example.invalid/app/index.html?mode=test#shell",
            "origin": "https://example.invalid",
            "pathname": "/app/index.html",
            "search": "?mode=test",
            "hash": "#shell",
        })]
    );
}

#[tokio::test]
async fn emulator_launch_is_acknowledged_asynchronously() {
    let html = r##"
        <html><body>
          <tracked-surface id="game"></tracked-surface>
          <script>
            const before = Snowglass.emulator.launch({
              command: "emulator.exe",
              arguments: [],
              isolated: false,
              hook: ["d3d12"],
              surfaces: [{ index: 0, rotation: 0, attach: document.querySelector("#game"), webrtc: false }],
              gamepadMapping: [],
              machine: "TEST",
            });
            postMessage({
              type: "pending",
              promise: before instanceof Promise,
              id: before.id,
              state: before.state,
            });
          </script>
        </body></html>
    "##;
    let document = document(html);
    let sink = EmulatorCommandSink::new(|command| {
        let EmulatorCommand::Launch { result, .. } = command else {
            panic!("unexpected emulator command")
        };
        result.send(Ok(())).unwrap();
    });
    let mut runtime = Runtime::new(
        document.as_ref(),
        html,
        "snowglass:///launch-test.html",
        800,
        600,
        sink,
    )
    .await
    .unwrap();
    runtime.poll_event_loop().await.unwrap();
    assert_eq!(
        runtime.take_messages(),
        [serde_json::json!({
            "type": "pending",
            "promise": false,
            "id": 1,
            "state": "pending",
        })]
    );
}

#[tokio::test]
async fn emulator_launch_rejects_and_clears_the_provisional_instance() {
    let html = r##"
        <html><body>
          <tracked-surface id="game"></tracked-surface>
          <script>
            const launch = Snowglass.emulator.launch({
              command: "emulator.exe",
              arguments: [],
              isolated: false,
              hook: ["d3d12"],
              surfaces: [{ index: 0, rotation: 0, attach: document.querySelector("#game"), webrtc: false }],
              gamepadMapping: [],
              machine: "TEST",
            });
            launch.ready.catch(error => postMessage({
              message: error.message,
              active: Snowglass.emulator.activeLaunch !== null,
            }));
          </script>
        </body></html>
    "##;
    let document = document(html);
    let sink = EmulatorCommandSink::new(|command| {
        let EmulatorCommand::Launch { result, .. } = command else {
            panic!("unexpected emulator command")
        };
        result.send(Err("native launch failed".to_owned())).unwrap();
    });
    let mut runtime = Runtime::new(
        document.as_ref(),
        html,
        "snowglass:///launch-failure-test.html",
        800,
        600,
        sink,
    )
    .await
    .unwrap();
    runtime.poll_event_loop().await.unwrap();
    assert_eq!(
        runtime.take_messages(),
        [serde_json::json!({
            "message": "native launch failed",
            "active": false,
        })]
    );
}

#[tokio::test]
async fn webrtc_facade_owns_publication_lifecycle_through_async_ops() {
    let html = r##"
        <html><body>
          <tracked-surface id="game"></tracked-surface>
          <script>
            (async () => {
              const instance = await Snowglass.emulator.launch({
                command: "emulator.exe",
                arguments: [],
                isolated: false,
                hook: ["d3d12"],
                surfaces: [{ index: 0, rotation: 0, attach: document.querySelector("#game"), webrtc: false }],
                gamepadMapping: [],
                machine: "TEST",
              });
              const publication = await Snowglass.webrtc.start({
                instance,
                bind: "127.0.0.1:9090",
                targets: [0],
              });
              postMessage({
                type: "started",
                state: publication.state,
                active: Snowglass.webrtc.activePublication === publication,
              });
              await publication.close();
              postMessage({
                type: "stopped",
                state: publication.state,
                active: Snowglass.webrtc.activePublication !== null,
              });
            })();
          </script>
        </body></html>
    "##;
    let document = document(html);
    let emulator_sink = EmulatorCommandSink::new(|command| {
        let EmulatorCommand::Launch { result, .. } = command else {
            panic!("unexpected emulator command")
        };
        result.send(Ok(())).unwrap();
    });
    let commands = Arc::new(Mutex::new(Vec::new()));
    let recorded = commands.clone();
    let webrtc_sink = WebRtcCommandSink::new(move |command| match command {
        WebRtcCommand::Start {
            token,
            bind,
            targets,
            result,
        } => {
            recorded.lock().unwrap().push(serde_json::json!({
                "type": "start",
                "token": token.0,
                "bind": bind.to_string(),
                "targets": targets,
            }));
            result.send(Ok(())).unwrap();
        }
        WebRtcCommand::Stop { token, result } => {
            recorded.lock().unwrap().push(serde_json::json!({
                "type": "stop",
                "token": token.0,
            }));
            result.send(Ok(())).unwrap();
        }
    });
    let plugin_runtime = Arc::new(snowglass_pluginhost::PluginRuntime::new(
        snowglass_pluginhost::PluginRegistry::new(),
        snowglass_pluginhost::PluginRuntimeOptions::default(),
    ));
    let mut runtime = Runtime::new_with_command_sinks(
        document.as_ref(),
        html,
        "snowglass:///webrtc-test.html",
        800,
        600,
        emulator_sink,
        InputCommandSink::default(),
        webrtc_sink,
        plugin_runtime,
    )
    .await
    .unwrap();
    runtime.poll_event_loop().await.unwrap();
    assert_eq!(
        runtime.take_messages(),
        [
            serde_json::json!({ "type": "started", "state": "active", "active": true }),
            serde_json::json!({ "type": "stopped", "state": "closed", "active": false }),
        ]
    );
    assert_eq!(
        *commands.lock().unwrap(),
        [
            serde_json::json!({
                "type": "start",
                "token": 1,
                "bind": "127.0.0.1:9090",
                "targets": [0],
            }),
            serde_json::json!({ "type": "stop", "token": 1 }),
        ]
    );
}

#[tokio::test]
async fn emulator_launch_delegates_webrtc_options_to_the_deno_namespace() {
    let html = r##"
        <html><body>
          <tracked-surface id="game"></tracked-surface>
          <script>
            const instance = Snowglass.emulator.launch({
              command: "emulator.exe",
              arguments: [],
              isolated: false,
              hook: ["d3d12"],
              surfaces: [{ index: 3, rotation: 0, attach: document.querySelector("#game"), webrtc: true }],
              gamepadMapping: [],
              machine: "TEST",
              webrtcBind: "127.0.0.1:9191",
            });
            Snowglass.webrtc.addEventListener("activepublicationchange", event => {
              event.detail.addEventListener("statechange", () => postMessage({
                instance: instance.id,
                publicationState: event.detail.state,
                targets: event.detail.targets,
              }), { once: true });
            }, { once: true });
          </script>
        </body></html>
    "##;
    let document = document(html);
    let emulator_sink = EmulatorCommandSink::new(|command| {
        let EmulatorCommand::Launch {
            options, result, ..
        } = command
        else {
            panic!("unexpected emulator command")
        };
        assert_eq!(options["surfaces"][0]["index"], 3);
        result.send(Ok(())).unwrap();
    });
    let webrtc_sink = WebRtcCommandSink::new(|command| {
        let WebRtcCommand::Start {
            token,
            bind,
            targets,
            result,
        } = command
        else {
            panic!("unexpected WebRTC command")
        };
        assert_eq!(token.0, 1);
        assert_eq!(bind.to_string(), "127.0.0.1:9191");
        assert_eq!(targets, [3]);
        result.send(Ok(())).unwrap();
    });
    let plugin_runtime = Arc::new(snowglass_pluginhost::PluginRuntime::new(
        snowglass_pluginhost::PluginRegistry::new(),
        snowglass_pluginhost::PluginRuntimeOptions::default(),
    ));
    let mut runtime = Runtime::new_with_command_sinks(
        document.as_ref(),
        html,
        "snowglass:///webrtc-launch-test.html",
        800,
        600,
        emulator_sink,
        InputCommandSink::default(),
        webrtc_sink,
        plugin_runtime,
    )
    .await
    .unwrap();
    runtime.poll_event_loop().await.unwrap();
    assert_eq!(
        runtime.take_messages(),
        [serde_json::json!({
            "instance": 1,
            "publicationState": "active",
            "targets": [3],
        })]
    );
}

#[tokio::test]
async fn plugin_facade_rejects_non_json_arguments_before_invocation() {
    let html = r#"
        <html><body><script>
          Snowglass.plugin.open("missing").call(1n)
            .catch(error => postMessage({ name: error.name, message: error.message }));
        </script></body></html>
    "#;
    let document = document(html);
    let mut runtime = Runtime::new(
        document.as_ref(),
        html,
        "snowglass:///plugin-json-test.html",
        800,
        600,
        EmulatorCommandSink::default(),
    )
    .await
    .unwrap();
    runtime.poll_event_loop().await.unwrap();
    assert_eq!(
        runtime.take_messages(),
        [serde_json::json!({
            "name": "TypeError",
            "message": "plugin arguments must contain only JSON-compatible values",
        })]
    );
}

#[tokio::test]
async fn plugin_facade_invokes_a_fresh_worker_through_the_direct_op() {
    let plugins = snowglass_pluginhost::PluginRegistry::new();
    plugins
        .load(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("plugins")
                .join("cemu")
                .join("plugin.toml"),
        )
        .unwrap();
    let plugin_runtime = Arc::new(snowglass_pluginhost::PluginRuntime::new(
        plugins,
        snowglass_pluginhost::PluginRuntimeOptions::default(),
    ));
    let html = r#"
        <html><body><script>
          Snowglass.plugin.open("cemu").setupLaunch({
            launchPath: "E:/games/U-King.rpx",
            console: "NINTENDO_WIIU",
            portAssignments: { gamepad: ["WIIU_DRC_CONTROLLER"] },
          }).then(plan => postMessage({ command: plan.command, hook: plan.hooks[0] }));
        </script></body></html>
    "#;
    let document = document(html);
    let mut runtime = Runtime::new_with_plugin_runtime(
        document.as_ref(),
        html,
        "snowglass:///plugin-test.html",
        800,
        600,
        EmulatorCommandSink::default(),
        plugin_runtime,
    )
    .await
    .unwrap();
    let mut messages = Vec::new();
    for _ in 0..1_000 {
        runtime.poll_event_loop().await.unwrap();
        messages = runtime.take_messages();
        if !messages.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    assert_eq!(
        messages.len(),
        1,
        "plugin result was not delivered: {messages:?}"
    );
    assert!(
        messages[0]["command"]
            .as_str()
            .unwrap()
            .ends_with(r"\Cemu\Cemu.exe")
    );
    assert_eq!(messages[0]["hook"], "vulkan");
}

#[tokio::test]
async fn runs_inline_modules_with_relative_imports() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("dependency.js"),
        "window.__dependencyUsedWindow = true; export const answer = 42;",
    )
    .unwrap();
    let document_url =
        deno_core::ModuleSpecifier::from_file_path(directory.path().join("shell.html")).unwrap();
    let html = r#"
        <html><body>
          <script type="module">
            import { answer } from "./dependency.js";
            const fetched = await (await fetch("data:text/plain,ready")).text();
            postMessage({
              type: "module-ran",
              answer,
              fetched,
              nativeEvent: typeof Event.prototype.composedPath === "function",
              nativeFetch: typeof Response === "function",
              dependencyUsedWindow: window.__dependencyUsedWindow,
              browserGlobals:
                window === globalThis &&
                self === globalThis &&
                parent === globalThis &&
                top === globalThis,
              windowScroll:
                typeof window.scrollTo === "function" &&
                typeof window.scrollBy === "function" &&
                window.scrollX === 0 &&
                window.scrollY === 0,
              observers:
                typeof ResizeObserver === "function" &&
                typeof IntersectionObserver === "function",
            });
            setTimeout(() => postMessage({ type: "timer-ran" }), 50);
          </script>
        </body></html>
    "#;
    let document = document(html);
    let mut runtime = Runtime::new(
        document.as_ref(),
        html,
        document_url.as_str(),
        800,
        600,
        EmulatorCommandSink::default(),
    )
    .await
    .unwrap();

    assert_eq!(
        runtime.take_messages(),
        [serde_json::json!({
            "type": "module-ran",
            "answer": 42,
            "fetched": "ready",
            "nativeEvent": true,
            "nativeFetch": true,
            "dependencyUsedWindow": true,
            "browserGlobals": true,
            "windowScroll": true,
            "observers": true,
        })]
    );
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    runtime.poll_event_loop().await.unwrap();
    assert_eq!(
        runtime.take_messages(),
        [serde_json::json!({ "type": "timer-ran" })]
    );
}

#[tokio::test]
async fn queues_dynamic_dom_tree_mutations() {
    let html = r##"
        <html><body><div id="root"></div>
          <script>
            const section = document.createElement("section");
            section.className = "surface-stage";
            const surface = document.createElement("tracked-surface");
            surface.setAttribute("src", "0");
            const label = document.createTextNode("screen");
            section.append(surface, label);
            const input = document.createElement("input");
            section.append(input);
            document.querySelector("#root").appendChild(section);
            input.focus();
            postMessage({
                type: "dynamic-tree",
                surface: document.querySelector("tracked-surface").getAttribute("src"),
                label: section.textContent,
                focused: document.activeElement === input,
            });
          </script>
        </body></html>
    "##;
    let document = document(html);
    let mut runtime = Runtime::new(
        document.as_ref(),
        html,
        "snowglass:///dynamic.html",
        800,
        600,
        EmulatorCommandSink::default(),
    )
    .await
    .unwrap();
    let mutations = runtime.take_mutations();

    assert!(
        mutations.iter().any(
            |mutation| matches!(mutation, Mutation::CreateElement { tag, .. } if tag == "tracked-surface")
        ),
        "the tracked surface was not created: {mutations:?}"
    );
    assert!(
        mutations
            .iter()
            .any(|mutation| matches!(mutation, Mutation::Focus { .. })),
        "programmatic focus was not transported: {mutations:?}"
    );
    assert!(
        mutations
            .iter()
            .filter(|mutation| matches!(mutation, Mutation::AppendChild { .. }))
            .count()
            >= 3,
        "the dynamic tree was not connected: {mutations:?}"
    );
    assert_eq!(
        runtime.take_messages(),
        [serde_json::json!({
            "type": "dynamic-tree",
            "surface": "0",
            "label": "screen",
            "focused": true,
        })]
    );
}

#[tokio::test]
async fn bounding_client_rect_reflects_standard_inline_transform_synchronously() {
    let html = r##"
        <html><body>
          <div id="panel" style="position:fixed;top:0;left:0;width:200px;height:100px">
            <div id="handle" style="width:50px;height:20px"></div>
          </div>
          <script>
            const panel = document.querySelector("#panel");
            const handle = document.querySelector("#handle");
            panel.style.setProperty("transform", "translate(125px, 75px)");
            const panelRect = panel.getBoundingClientRect();
            const handleRect = handle.getBoundingClientRect();
            postMessage({
              type: "translated-geometry",
              panel: [panelRect.x, panelRect.y, panelRect.width, panelRect.height],
              handle: [handleRect.x, handleRect.y],
            });
          </script>
        </body></html>
    "##;
    let document = document(html);
    let mut runtime = Runtime::new(
        document.as_ref(),
        html,
        "snowglass:///translated-geometry.html",
        800,
        600,
        EmulatorCommandSink::default(),
    )
    .await
    .unwrap();

    assert_eq!(
        runtime.take_messages(),
        [serde_json::json!({
            "type": "translated-geometry",
            // The document's default 8px body margin remains part of layout;
            // the inline translation is added to it just as it is in a browser.
            "panel": [133, 83, 200, 100],
            "handle": [133, 83],
        })]
    );
}

#[tokio::test]
async fn pointer_capture_retargets_native_pointer_moves() {
    let html = r##"
        <html><body>
          <div id="capture"></div><div id="native-hit"></div>
          <script>
            const capture = document.querySelector("#capture");
            capture.addEventListener("pointermove", () => postMessage({ type: "captured-move" }));
            capture.setPointerCapture(1);
            postMessage({ type: "capture-state", captured: capture.hasPointerCapture(1) });
          </script>
        </body></html>
    "##;
    let document = document(html);
    let native_hit = document
        .as_ref()
        .tree()
        .iter()
        .find_map(|(id, node)| {
            (node.attr(blitz_dom::local_name!("id")) == Some("native-hit")).then_some(id)
        })
        .unwrap();
    let mut runtime = Runtime::new(
        document.as_ref(),
        html,
        "snowglass:///pointer-capture.html",
        800,
        600,
        EmulatorCommandSink::default(),
    )
    .await
    .unwrap();

    runtime
        .dispatch_dom(
            "pointermove",
            native_hit,
            "pointer",
            &serde_json::json!({ "pointerId": 1, "clientX": 20, "clientY": 30 }),
            true,
            true,
        )
        .unwrap();
    runtime.poll_event_loop().await.unwrap();

    assert_eq!(
        runtime.take_messages(),
        [
            serde_json::json!({
                "type": "capture-state",
                "captured": true,
            }),
            serde_json::json!({
                "type": "captured-move",
            })
        ]
    );
}

#[tokio::test]
async fn parses_clones_and_mounts_solid_style_html_and_svg_templates() {
    let html = r##"
        <html><body><script>
          const template = document.createElement("template");
          template.innerHTML = `<section class="card"><span>Ready</span><svg viewBox="0 0 8 8"><path d="M0 0h8v8z"></path></svg></section>`;
          const clone = template.content.cloneNode(true);
          document.body.append(clone);
          const section = document.querySelector(".card");
          const svg = section.querySelector("svg");
          postMessage({
            type: "template",
            text: section.textContent,
            childCount: section.children.length,
            svgNamespace: svg.namespaceURI,
            connected: section.isConnected,
          });
        </script></body></html>
    "##;
    let document = document(html);
    let mut runtime = Runtime::new(
        document.as_ref(),
        html,
        "snowglass:///template.html",
        800,
        600,
        EmulatorCommandSink::default(),
    )
    .await
    .unwrap();
    let mutations = runtime.take_mutations();

    assert!(
        mutations.iter().any(|mutation| matches!(
            mutation,
            Mutation::CreateElement { tag, namespace: Some(namespace), .. }
                if tag == "svg" && namespace == "http://www.w3.org/2000/svg"
        )),
        "the SVG template did not retain its namespace: {mutations:?}"
    );
    assert_eq!(
        runtime.take_messages(),
        [serde_json::json!({
            "type": "template",
            "text": "Ready",
            "childCount": 2,
            "svgNamespace": "http://www.w3.org/2000/svg",
            "connected": true,
        })]
    );
}
