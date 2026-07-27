use blitz_dom::{DocumentConfig, local_name};

use super::{Command, MouseEvent, Mutation, Runtime};

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
    let mut runtime =
        Runtime::new(document.as_ref(), html, document_url.as_str(), 800, 600).unwrap();

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
            const instance = gameView.launch({
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
            postMessage({
              type: "attached",
              launched: instance !== null,
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
    let mut runtime =
        Runtime::new(document.as_ref(), html, "snowglass:///test.html", 800, 600).unwrap();

    assert!(matches!(
        runtime.take_commands().as_slice(),
        [Command::Launch { options, .. }]
            if options["surfaces"][0]["attach"] == serde_json::json!(surface_node)
    ));
    assert_eq!(
        runtime.take_messages(),
        [serde_json::json!({
            "type": "attached",
            "launched": true,
            "tracked": true,
        })]
    );

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
    runtime.take_proxy_messages();
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
}
