use blitz_dom::{DocumentConfig, local_name};
use std::sync::{Arc, Mutex};

use super::{EmulatorCommand, EmulatorCommandSink, KeyboardEvent, MouseEvent, Mutation, Runtime};

fn command_recorder() -> (EmulatorCommandSink, Arc<Mutex<Vec<EmulatorCommand>>>) {
    let commands = Arc::new(Mutex::new(Vec::new()));
    let recorded = commands.clone();
    (
        EmulatorCommandSink::new(move |command| recorded.lock().unwrap().push(command)),
        commands,
    )
}

fn drain_recorded_commands(commands: &Mutex<Vec<EmulatorCommand>>) -> Vec<EmulatorCommand> {
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

#[test]
fn background_input_api_sends_revisioned_transactions_and_resolves_results() {
    let html = r#"
        <html><body>
          <script>
            snowglassInput.configure({
              operations: [{ type: "resetPortOverrides", port: 0 }]
            }).then((configuration) => {
              postMessage({
                type: "input-configured",
                revision: configuration.revision
              });
            });
          </script>
        </body></html>
    "#;
    let document = document(html);
    let (command_sink, commands) = command_recorder();
    let mut runtime = Runtime::new(
        document.as_ref(),
        html,
        "snowglass:///input-test.html",
        800,
        600,
        command_sink,
    )
    .unwrap();

    assert!(matches!(
        drain_recorded_commands(&commands).as_slice(),
        [EmulatorCommand::ConfigureInput {
            request_id: 1,
            transaction,
        }] if transaction.expected_revision == 0
            && matches!(
                transaction.operations.as_slice(),
                [snowglass_input::hub::InputOperation::ResetPortOverrides { port }]
                    if port.0 == 0
            )
    ));

    runtime
        .input_configuration_result(
            1,
            &serde_json::json!({
                "ok": true,
                "configuration": {
                    "revision": 1,
                    "stoneVersion": "12.0.0",
                    "ports": []
                }
            }),
        )
        .unwrap();
    assert_eq!(
        runtime.take_messages(),
        [serde_json::json!({
            "type": "input-configured",
            "revision": 1,
        })]
    );
}

#[test]
fn runs_inline_modules_with_relative_imports() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("dependency.js"),
        "window.__dependencyUsedWindow = true; export const answer = 42;",
    )
    .unwrap();
    let document_url =
        deno_core::ModuleSpecifier::from_file_path(directory.path().join("game-layer.html"))
            .unwrap();
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
        })]
    );
    std::thread::sleep(std::time::Duration::from_millis(60));
    runtime.poll_event_loop().unwrap();
    assert_eq!(
        runtime.take_messages(),
        [serde_json::json!({ "type": "timer-ran" })]
    );
}

#[test]
fn queues_dynamic_dom_tree_mutations() {
    let html = r##"
        <html><body><div id="root"></div>
          <script>
            const section = document.createElement("section");
            section.className = "surface-stage";
            const surface = document.createElement("tracked-surface");
            surface.setAttribute("src", "0");
            const label = document.createTextNode("screen");
            section.append(surface, label);
            document.querySelector("#root").appendChild(section);
            postMessage({
              type: "dynamic-tree",
              surface: document.querySelector("tracked-surface").getAttribute("src"),
              label: section.textContent,
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
        })]
    );
}

#[test]
fn bridges_dom_events_games_messages_and_remote_objects() {
    let html = r##"
        <html><body>
          <div id="handle"><tracked-surface src="0"></tracked-surface></div>
          <script>
            const handle = document.querySelector("#handle");
            const order = [];
            document.addEventListener("mousedown", () => order.push("document-capture"), true);
            addEventListener("mousedown", () => {
              order.push("window-bubble");
              postMessage({ type: "propagation", order });
            });
            addEventListener("keydown", event => {
              postMessage({
                type: "keyboard",
                key: event.key,
                code: event.code,
                altKey: event.altKey,
              });
              if (event.altKey && event.code === "Enter") event.preventDefault();
            });
            addEventListener("resize", () => {
              postMessage({ type: "resize", width: innerWidth, height: innerHeight });
            });
            onmessage = event => postMessage({
              type: "background-message",
              value: event.data.value,
            });
            window.proxyFixture = {
              value: "background-value",
              sum(left, right) {
                return left + right;
              },
              acceptsTrackedSurface(surface) {
                return surface instanceof HTMLTrackedSurfaceElement;
              },
              fire(value) {
                return dispatchEvent(new CustomEvent("proxy-ping", { detail: value }));
              },
            };
            handle.addEventListener("mousedown", event => {
              order.push("target");
              handle.style.setProperty("--moved", "12px");
              postMessage({
                type: "pressed",
                id: event.currentTarget.id,
                tracked: document.querySelector("tracked-surface")
                  instanceof HTMLTrackedSurfaceElement,
              });
              event.preventDefault();
            });
            const surface = document.querySelector("tracked-surface");
            Emulator.addEventListener("activeinstancechange", () => {
              const active = Emulator.activeInstance;
              if (!active) return;
              active.on("ready", () => postMessage({ type: "emulator-ready" }));
              active.on("surfacesChanged", surfaces => postMessage({
                type: "emulator-surfaces",
                count: surfaces.length,
                width: surfaces[0]?.width,
              }));
            });
            const instance = Emulator.launch({
              command: "game.exe",
              arguments: [],
              isolated: false,
              hook: ["d3d12"],
              surfaces: [{
                index: 0,
                rotation: 0,
                attach: surface,
                webrtc: false,
              }],
              gamepadMapping: [],
            });
            instance.setVolume(0.35);
            instance.setMuted(true);
            postMessage({
              type: "attached",
              launched: instance !== null,
              active: Emulator.activeInstance === instance,
              state: instance.state,
              tracked: surface instanceof HTMLTrackedSurfaceElement,
            });
          </script>
        </body></html>
    "##;
    let document = document(html);
    let handle = document
        .as_ref()
        .tree()
        .iter()
        .find_map(|(id, node)| (node.attr(local_name!("id")) == Some("handle")).then_some(id))
        .unwrap();
    let surface_node = document
        .as_ref()
        .tree()
        .iter()
        .find_map(|(id, node)| {
            node.element_data()
                .is_some_and(|element| element.name.local.as_ref() == "tracked-surface")
                .then_some(id)
        })
        .unwrap();
    let (command_sink, commands) = command_recorder();
    let mut runtime = Runtime::new(
        document.as_ref(),
        html,
        "snowglass:///test.html",
        800,
        600,
        command_sink,
    )
    .unwrap();

    let initial_commands = drain_recorded_commands(&commands);
    assert!(matches!(
        initial_commands.as_slice(),
        [
            EmulatorCommand::Launch {
                token,
                options,
            },
            EmulatorCommand::SetVolume {
                token: volume_token,
                volume,
            },
            EmulatorCommand::SetMuted {
                token: muted_token,
                muted: true,
            },
        ] if options.surfaces[0].attach == Some(surface_node)
            && token == volume_token
            && token == muted_token
            && (*volume - 0.35).abs() < f64::EPSILON
    ));
    assert_eq!(
        runtime.take_messages(),
        [serde_json::json!({
            "type": "attached",
            "launched": true,
            "active": true,
            "state": "starting",
            "tracked": true,
        })]
    );

    runtime
        .dispatch_emulator_event(&serde_json::json!({
            "instanceId": 1,
            "eventName": "ready",
            "payload": null,
        }))
        .unwrap();
    runtime
        .dispatch_emulator_event(&serde_json::json!({
            "instanceId": 1,
            "eventName": "surfacesChanged",
            "payload": [{
                "connection": "7",
                "index": 0,
                "width": 1280,
                "height": 720,
            }],
        }))
        .unwrap();
    assert_eq!(
        runtime.take_messages(),
        [
            serde_json::json!({ "type": "emulator-ready" }),
            serde_json::json!({
                "type": "emulator-surfaces",
                "count": 1,
                "width": 1280,
            }),
        ]
    );
    runtime.take_mutations();

    let result = runtime
        .dispatch_mouse(
            "mousedown",
            handle,
            MouseEvent {
                client_x: 10.0,
                client_y: 10.0,
                button: 0,
                buttons: 1,
                ctrl_key: false,
                shift_key: false,
                alt_key: false,
                meta_key: false,
            },
        )
        .unwrap();
    assert!(result.handled);
    assert!(result.default_prevented);
    assert!(matches!(
        runtime.take_mutations().as_slice(),
        [Mutation::SetAttribute { name, value, .. }]
            if name == "style" && value.contains("--moved: 12px")
    ));
    assert_eq!(
        runtime.take_messages(),
        [
            serde_json::json!({
                "type": "pressed",
                "id": "handle",
                "tracked": true,
            }),
            serde_json::json!({
                "type": "propagation",
                "order": ["document-capture", "target", "window-bubble"],
            }),
        ]
    );

    let result = runtime
        .dispatch_keyboard(
            "keydown",
            KeyboardEvent {
                key: "Enter".into(),
                code: "Enter".into(),
                repeat: false,
                alt_key: true,
                ctrl_key: false,
                shift_key: false,
                meta_key: false,
            },
        )
        .unwrap();
    assert!(result.handled);
    assert!(result.default_prevented);
    assert_eq!(
        runtime.take_messages(),
        [serde_json::json!({
            "type": "keyboard",
            "key": "Enter",
            "code": "Enter",
            "altKey": true,
        })]
    );

    runtime.resize(1024, 768).unwrap();
    assert_eq!(
        runtime.take_messages(),
        [serde_json::json!({
            "type": "resize",
            "width": 1024,
            "height": 768,
        })]
    );

    runtime
        .post_message(&serde_json::json!({ "value": "from-foreground" }))
        .unwrap();
    assert_eq!(
        runtime.take_messages(),
        [serde_json::json!({
            "type": "background-message",
            "value": "from-foreground",
        })]
    );

    runtime
        .proxy_request(&serde_json::json!({
            "id": 0,
            "operation": "apply",
            "handle": 0,
            "path": ["Emulator", "activeInstance", "setVolume"],
            "args": [{ "kind": "value", "value": 0.6 }],
        }))
        .unwrap();
    assert_eq!(
        runtime.take_proxy_messages(),
        [serde_json::json!({
            "type": "response",
            "id": 0,
            "ok": true,
            "value": { "kind": "undefined" },
        })]
    );
    let volume_commands = drain_recorded_commands(&commands);
    assert!(matches!(
        volume_commands.as_slice(),
        [EmulatorCommand::SetVolume { volume, .. }] if (*volume - 0.6).abs() < f64::EPSILON
    ));

    runtime
        .proxy_request(&serde_json::json!({
            "id": 1,
            "operation": "apply",
            "handle": 0,
            "path": ["proxyFixture", "sum"],
            "args": [
                { "kind": "value", "value": 20 },
                { "kind": "value", "value": 22 },
            ],
        }))
        .unwrap();
    assert_eq!(
        runtime.take_proxy_messages(),
        [serde_json::json!({
            "type": "response",
            "id": 1,
            "ok": true,
            "value": { "kind": "value", "value": 42 },
        })]
    );

    runtime
        .proxy_request(&serde_json::json!({
            "id": 2,
            "operation": "apply",
            "handle": 0,
            "path": ["document", "querySelector"],
            "args": [{ "kind": "value", "value": "tracked-surface" }],
        }))
        .unwrap();
    let surface_response = runtime.take_proxy_messages().pop().unwrap();
    let surface_handle = surface_response["value"]["handle"].as_u64().unwrap();
    assert_eq!(surface_response["value"]["kind"], "backgroundReference");

    runtime
        .proxy_request(&serde_json::json!({
            "id": 3,
            "operation": "apply",
            "handle": 0,
            "path": ["proxyFixture", "acceptsTrackedSurface"],
            "args": [{
                "kind": "backgroundReference",
                "handle": surface_handle,
            }],
        }))
        .unwrap();
    assert_eq!(
        runtime.take_proxy_messages()[0]["value"],
        serde_json::json!({ "kind": "value", "value": true })
    );

    runtime
        .proxy_request(&serde_json::json!({
            "id": 4,
            "operation": "apply",
            "handle": 0,
            "path": ["addEventListener"],
            "args": [
                { "kind": "value", "value": "proxy-ping" },
                { "kind": "foregroundCallback", "handle": 11 },
            ],
        }))
        .unwrap();
    let registration = runtime.take_proxy_messages();
    assert_eq!(
        registration[0]["ok"],
        serde_json::json!(true),
        "{registration:?}"
    );
    runtime
        .proxy_request(&serde_json::json!({
            "id": 5,
            "operation": "apply",
            "handle": 0,
            "path": ["proxyFixture", "fire"],
            "args": [{ "kind": "value", "value": "callback-value" }],
        }))
        .unwrap();
    let proxy_messages = runtime.take_proxy_messages();
    assert_eq!(proxy_messages[0]["type"], "callback");
    assert_eq!(proxy_messages[0]["handle"], 11);
    assert_eq!(proxy_messages[1]["type"], "response");

    runtime
        .proxy_request(&serde_json::json!({
            "id": 6,
            "operation": "apply",
            "handle": 0,
            "path": ["Emulator", "addEventListener"],
            "args": [
                { "kind": "value", "value": "activeinstancechange" },
                { "kind": "foregroundCallback", "handle": 12 },
            ],
        }))
        .unwrap();
    let emulator_registration = runtime.take_proxy_messages();
    assert_eq!(
        emulator_registration[0]["ok"],
        serde_json::json!(true),
        "{emulator_registration:?}"
    );
    runtime
        .proxy_request(&serde_json::json!({
            "id": 7,
            "operation": "get",
            "handle": 0,
            "path": ["Emulator", "activeInstance"],
        }))
        .unwrap();
    let active_response = runtime.take_proxy_messages();
    let active_handle = active_response[0]["value"]["handle"].as_u64().unwrap();
    runtime
        .proxy_request(&serde_json::json!({
            "id": 8,
            "operation": "apply",
            "handle": active_handle,
            "path": ["activateInput"],
            "args": [],
        }))
        .unwrap();
    runtime.take_proxy_messages();
    assert!(matches!(
        drain_recorded_commands(&commands).as_slice(),
        [EmulatorCommand::Input {
            token,
            active: true,
        }] if token.0 == 1
    ));
    runtime
        .dispatch_emulator_event(&serde_json::json!({
            "instanceId": 1,
            "eventName": "closed",
            "payload": null,
        }))
        .unwrap();
    let active_change = runtime.take_proxy_messages();
    assert_eq!(active_change[0]["type"], "callback");
    assert_eq!(active_change[0]["handle"], 12);
    runtime
        .proxy_request(&serde_json::json!({
            "id": 9,
            "operation": "apply",
            "handle": active_handle,
            "path": ["setVolume"],
            "args": [{ "kind": "value", "value": 0.2 }],
        }))
        .unwrap();
    runtime.take_proxy_messages();
    assert!(drain_recorded_commands(&commands).is_empty());
    runtime
        .proxy_request(&serde_json::json!({
            "id": 10,
            "operation": "get",
            "handle": 0,
            "path": ["Emulator", "activeInstance"],
        }))
        .unwrap();
    assert_eq!(
        runtime.take_proxy_messages(),
        [serde_json::json!({
            "type": "response",
            "id": 10,
            "ok": true,
            "value": { "kind": "value", "value": null },
        })]
    );
}
