use super::*;
use crate::shell_runtime::deno;
use anyrender::Scene;
use blitz_dom::LocalName;
use std::sync::Arc;

fn image_fill_count(scene: &anyrender::Scene) -> usize {
    scene
        .commands
        .iter()
        .filter(|command| {
            matches!(
                command,
                anyrender::recording::RenderCommand::Fill(fill)
                    if matches!(fill.brush, anyrender::Paint::Image(_))
            )
        })
        .count()
}

#[test]
fn repeated_raster_background_survives_viewport_resize() {
    let mut document = blitz_html::HtmlDocument::from_html(
        r#"<style>
            html, body { width: 100%; height: 100%; margin: 0; }
            .backdrop {
                position: absolute;
                inset: 0;
                background-color: #050505;
                background-image: url("snowglass:///dots.png"), linear-gradient(90deg, #292929, #050505);
                background-repeat: repeat, no-repeat;
                background-size: 24px 24px, 100% 100%;
            }
        </style><div class="backdrop"></div>"#,
        blitz_dom::DocumentConfig::default(),
    );
    let viewport = |width, height| {
        blitz_traits::shell::Viewport::new(
            width,
            height,
            1.0,
            blitz_traits::shell::ColorScheme::Dark,
        )
    };
    document.as_mut().set_viewport(viewport(800, 600));
    document.as_mut().resolve(0.0);
    document
        .as_mut()
        .load_resource(blitz_dom::net::ResourceLoadResponse {
            request_id: 0,
            node_id: None,
            resolved_url: Some("snowglass:///dots.png".to_owned()),
            result: Ok(blitz_dom::net::Resource::Image(
                blitz_dom::util::ImageType::Background(0),
                2,
                2,
                Arc::new(vec![0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255]),
            )),
        });
    document.as_mut().resolve(0.0);

    let paint = |document: &mut blitz_html::HtmlDocument, width, height| {
        let mut scene = anyrender::Scene::new();
        blitz_paint::paint_scene(&mut scene, document.as_mut(), 1.0, width, height, 0, 0);
        scene
    };
    let initial = paint(&mut document, 800, 600);
    assert!(image_fill_count(&initial) > 0);

    document.as_mut().set_viewport(viewport(1280, 720));
    document.as_mut().resolve(1.0);
    let resized = paint(&mut document, 1280, 720);
    assert!(image_fill_count(&resized) > 0);
}

#[test]
fn dpi_helpers_keep_css_and_compositor_coordinates_distinct() {
    assert_eq!(normalized_scale_factor(f32::NAN), 1.0);
    assert_eq!(normalized_scale_factor(0.0), 1.0);
    assert_eq!(normalized_scale_factor(1.5), 1.5);
    assert_eq!(
        scaled_rect(kurbo::Rect::new(10.0, 20.0, 110.0, 70.0), 1.5),
        kurbo::Rect::new(15.0, 30.0, 165.0, 105.0),
    );
}

#[test]
fn promoted_chrome_keeps_its_origin_outside_the_viewport() {
    let viewport = kurbo::Rect::new(0.0, 0.0, 800.0, 600.0);
    let bounds = chrome_fragment_bounds(
        Some(kurbo::Rect::new(-120.25, -40.5, 399.75, 299.5)),
        viewport,
    );
    assert_eq!(bounds, kurbo::Rect::new(-122.0, -42.0, 401.0, 301.0));
    assert_eq!(chrome_fragment_bounds(None, viewport), viewport);
}

#[test]
fn tracked_surface_becomes_a_custom_paint_placeholder() {
    let mut document = blitz_html::HtmlDocument::from_html(
        r#"<style>.panel {
                opacity: 0.4;
                background: red;
                box-shadow: 0 16px 40px black;
            }
            .surface {
                display: block;
                width: 320px;
                height: 180px;
                object-fit: cover;
                object-position: 25% 75%;
            }</style><div class="panel"><tracked-surface class="surface" src="7" data-corner-radius="12"></tracked-surface></div>"#,
        blitz_dom::DocumentConfig::default(),
    );
    initialise_tracked_surfaces(&mut document);
    document
        .as_mut()
        .set_viewport(blitz_traits::shell::Viewport::new(
            800,
            600,
            1.0,
            blitz_traits::shell::ColorScheme::Dark,
        ));
    document.as_mut().resolve(1.0);
    let (surface_id, surface) = document
        .as_ref()
        .tree()
        .iter()
        .find_map(|(node_id, node)| {
            node.element_data()
                .filter(|element| is_tracked_surface_element(element))
                .map(|element| (node_id, element))
        })
        .unwrap();
    assert!(surface.custom_widget_data().is_some());
    let mut scenes = vec![Scene::new()];
    let mut placements = Vec::new();
    let mut scene = BandedScene::new(&mut scenes, &mut placements);
    blitz_paint::paint_scene(&mut scene, document.as_mut(), 1.0, 800, 600, 0, 0);
    let output = scene.take_compositing_output();
    assert_eq!(scene.bands_used(), 2);
    let fragments = output.fragments;
    let groups = output.groups;
    drop(scene);
    assert_eq!(scenes.len(), 2);
    apply_tracked_surface_styles(document.as_ref(), &mut placements);
    assert_eq!(placements.len(), 1);
    assert_eq!(placements[0].node_id, Some(surface_id));
    assert_eq!(placements[0].source_id, 7);
    assert_eq!(placements[0].rect.width(), 320.0);
    assert_eq!(placements[0].rect.height(), 180.0);
    let clipping = placements[0]
        .clipping
        .expect("the containing paint layer publishes its clip");
    assert!(clipping.contains(placements[0].rect.origin()));
    assert_eq!(placements[0].corner_radius, 12.0);
    assert!((placements[0].opacity - 0.4).abs() < f32::EPSILON);
    assert!(placements[0].group.is_some());
    assert!(
        fragments
            .iter()
            .any(|fragment| fragment.group == placements[0].group)
    );
    let panel_fragment = fragments
        .iter()
        .find(|fragment| fragment.group == placements[0].group)
        .expect("the promoted panel has an independent chrome fragment");
    let panel_bounds = panel_fragment
        .bounds
        .expect("a promoted panel publishes tight compositor bounds");
    assert!(panel_bounds.width() < 800.0);
    assert!(panel_bounds.height() < 600.0);
    let shadow = groups
        .iter()
        .find(|group| Some(group.id) == placements[0].group)
        .and_then(|group| group.shadow)
        .expect("the outset shadow is owned by the native panel visual");
    assert_eq!(shadow.offset, [0.0, 16.0]);
    assert_eq!(shadow.blur_radius, 40.0);
    assert_eq!(shadow.color, [0, 0, 0, 255]);
    assert!(
        !panel_fragment.scene.commands.iter().any(|command| {
            matches!(command, anyrender::recording::RenderCommand::BoxShadow(_))
        })
    );
    let style = placements[0].style.as_ref().unwrap();
    assert_eq!(
        style.object_fit,
        style::properties::generated::longhands::object_fit::computed_value::T::Cover
    );
    assert_eq!(
        style
            .object_position
            .horizontal
            .resolve(style::values::computed::CSSPixelLength::new(100.0))
            .px(),
        25.0
    );
    assert_eq!(
        style
            .object_position
            .vertical
            .resolve(style::values::computed::CSSPixelLength::new(100.0))
            .px(),
        75.0
    );
}

#[test]
fn tracked_surface_group_owns_explicit_clip_path_chrome() {
    let mut document = blitz_html::HtmlDocument::from_html(
        r#"<style>
            html, body { width: 100%; height: 100%; margin: 0; }
            .positioner {
                position: fixed; top: 0; left: 0; width: 520px; height: 340px;
                transform: translate(300px, 100px); contain: layout paint;
            }
            .content {
                position: relative; width: 100%; height: 100%; overflow: hidden;
                clip-path: inset(0 round 12px); background: red;
            }
            .header { height: 32px; background: blue; }
            .body { position: relative; height: calc(100% - 32px); }
            tracked-surface { position: absolute; inset: 0; width: 100%; height: 100%; }
        </style>
        <div class="positioner"><div class="content">
            <div class="header">Gamepad display</div>
            <div class="body"><tracked-surface src="1"></tracked-surface></div>
        </div></div>"#,
        blitz_dom::DocumentConfig::default(),
    );
    initialise_tracked_surfaces(&mut document);
    document
        .as_mut()
        .set_viewport(blitz_traits::shell::Viewport::new(
            1280,
            720,
            1.0,
            blitz_traits::shell::ColorScheme::Dark,
        ));
    document.as_mut().resolve(1.0);

    let mut scenes = (0..3).map(|_| Scene::new()).collect::<Vec<_>>();
    let mut placements = Vec::new();
    let mut scene = BandedScene::new(&mut scenes, &mut placements);
    blitz_paint::paint_scene(&mut scene, document.as_mut(), 1.0, 1280, 720, 0, 0);
    let output = scene.take_compositing_output();
    let fragments = output.fragments;

    let placement = placements.first().expect("tracked surface placement");
    let group = placement.group.expect("enclosing compositor group");
    let chrome = fragments
        .iter()
        .find(|fragment| fragment.group == Some(group))
        .expect("the surface group also owns panel chrome");
    let bounds = chrome.bounds.expect("promoted panel has finite bounds");
    assert!(bounds.y0 < placement.rect.y0);
    assert!(bounds.height() > placement.rect.height());
}

#[test]
fn pointer_hit_skips_disabled_overlays_but_keeps_reenabled_children() {
    let html = r#"
            <style>
              :root, body { width: 100%; height: 100%; margin: 0; }
              .surface, .overlay, .shield { position: absolute; inset: 0; }
              .overlay { z-index: 20; pointer-events: none; }
              .shield { pointer-events: none; }
              .button {
                position: absolute;
                left: 10px;
                top: 10px;
                width: 40px;
                height: 40px;
                pointer-events: auto;
              }
            </style>
            <tracked-surface class="surface" id="surface" src="0"></tracked-surface>
            <div class="overlay">
              <div class="shield"></div>
              <div class="button" id="button"></div>
            </div>
        "#;
    let mut document =
        blitz_html::HtmlDocument::from_html(html, blitz_dom::DocumentConfig::default());
    document
        .as_mut()
        .set_viewport(blitz_traits::shell::Viewport::new(
            800,
            600,
            1.0,
            blitz_traits::shell::ColorScheme::Dark,
        ));
    document.as_mut().resolve(1.0);
    let find = |id| {
        document
            .as_ref()
            .tree()
            .iter()
            .find_map(|(node, current)| {
                (current.attr(local_name!("id")) == Some(id)).then_some(node)
            })
            .unwrap()
    };
    let surface = find("surface");
    let button = find("button");

    assert_eq!(pointer_hit(document.as_ref(), 20.0, 20.0), Some(button));
    assert_eq!(pointer_hit(document.as_ref(), 100.0, 100.0), Some(surface));
}

#[test]
fn pointer_hit_accounts_for_a_transformed_hoisted_panel() {
    let html = r#"
            <style>
              :root, body { width: 100%; height: 100%; margin: 0; }
              tracked-surface { position: absolute; inset: 0; }
              .panel {
                z-index: 20;
              }
              .header { display: flex; height: 32px; justify-content: flex-end; }
              button { width: 32px; height: 32px; }
            </style>
            <tracked-surface src="0"></tracked-surface>
            <div id="panel" class="panel" style="position: fixed; top: 0; left: 0; width: 520px; height: 340px; transform: translate(300px, 100px)"><div class="header"><button id="close">X</button></div></div>
        "#;
    let mut document =
        blitz_html::HtmlDocument::from_html(html, blitz_dom::DocumentConfig::default());
    document
        .as_mut()
        .set_viewport(blitz_traits::shell::Viewport::new(
            1280,
            832,
            1.0,
            blitz_traits::shell::ColorScheme::Dark,
        ));
    document.as_mut().resolve(1.0);
    let button = document
        .as_ref()
        .tree()
        .iter()
        .find_map(|(node, current)| {
            (current.attr(local_name!("id")) == Some("close")).then_some(node)
        })
        .unwrap();
    let panel = document
        .as_ref()
        .tree()
        .iter()
        .find_map(|(node, current)| {
            (current.attr(local_name!("id")) == Some("panel")).then_some(node)
        })
        .unwrap();

    assert_eq!(pointer_hit(document.as_ref(), 804.0, 116.0), Some(button));

    let mut script_to_dom = document
        .as_ref()
        .tree()
        .iter()
        .map(|(id, _)| (id, id))
        .collect::<HashMap<_, _>>();
    let mut dom_to_script = script_to_dom.clone();
    apply_dom_mutations(
            &mut document,
            &mut script_to_dom,
            &mut dom_to_script,
            vec![deno::Mutation::SetAttribute {
                node: panel,
                name: "style".to_owned(),
                value: "position: fixed; top: 0; left: 0; width: 520px; height: 340px; transform: translate(112px, 383px)".to_owned(),
            }],
        );
    document.as_mut().resolve(2.0);

    assert_eq!(pointer_hit(document.as_ref(), 616.0, 399.0), Some(button));
}

#[test]
fn pointer_hit_does_not_let_scroll_overflow_steal_a_header_button() {
    let html = r#"
            <style>
              :root, body { width: 100%; height: 100%; margin: 0; }
              main { display: flex; width: 100%; height: 100%; flex-direction: column; }
              header { display: flex; height: 76px; flex: none; justify-content: flex-end; }
              button { width: 92px; height: 48px; margin: 14px 22px; }
              .layout { display: flex; min-height: 0; flex: 1; }
              section { flex: 1; }
              aside { width: 352px; overflow: auto; }
              .tall { width: 100%; height: 16000px; }
            </style>
            <main>
              <header><button id="start">Start</button></header>
              <div class="layout"><section></section><aside><div class="tall"></div></aside></div>
            </main>
        "#;
    let mut document =
        blitz_html::HtmlDocument::from_html(html, blitz_dom::DocumentConfig::default());
    document
        .as_mut()
        .set_viewport(blitz_traits::shell::Viewport::new(
            1280,
            816,
            1.0,
            blitz_traits::shell::ColorScheme::Dark,
        ));
    document.as_mut().resolve(1.0);
    let button = document
        .as_ref()
        .tree()
        .iter()
        .find_map(|(node, current)| {
            (current.attr(local_name!("id")) == Some("start")).then_some(node)
        })
        .unwrap();
    let hit = pointer_hit(document.as_ref(), 1212.0, 38.0).unwrap();
    assert!(
        std::iter::successors(Some(hit), |node| document
            .as_ref()
            .get_node(*node)
            .and_then(|node| node.parent))
        .any(|node| node == button),
        "the header button was not in the hit target's event path"
    );
}

#[test]
fn paint_only_inline_styles_do_not_require_geometry_sync() {
    assert!(!style_change_requires_geometry(
        Some("transform: translate(10px, 20px); opacity: 0.5"),
        Some("transform: translate(30px, 40px); opacity: 0.8"),
    ));
    assert!(style_change_requires_geometry(
        Some("width: 100px; transform: translate(10px, 20px)"),
        Some("width: 120px; transform: translate(30px, 40px)"),
    ));
    assert!(style_change_requires_geometry(
        Some("--panel-x: 10px; transform: translate(10px, 20px)"),
        Some("--panel-x: 30px; transform: translate(30px, 40px)"),
    ));
}

#[test]
fn pointer_hit_finds_a_centered_dialog_action_over_a_surface() {
    let html = r#"
            <style>
              :root, body { width: 100%; height: 100%; margin: 0; }
              main, tracked-surface, .positioner { position: absolute; inset: 0; }
              .positioner { display: flex; align-items: center; justify-content: center; }
              .content { width: 260px; height: 150px; padding: 24px; }
              button { width: 90px; height: 28px; }
            </style>
            <main>
              <tracked-surface src="0"></tracked-surface>
              <div class="positioner"><div class="content"><button id="close">Close game</button></div></div>
            </main>
        "#;
    let mut document =
        blitz_html::HtmlDocument::from_html(html, blitz_dom::DocumentConfig::default());
    document
        .as_mut()
        .set_viewport(blitz_traits::shell::Viewport::new(
            800,
            600,
            1.0,
            blitz_traits::shell::ColorScheme::Dark,
        ));
    document.as_mut().resolve(1.0);
    let button = document
        .as_ref()
        .tree()
        .iter()
        .find_map(|(node, current)| {
            (current.attr(local_name!("id")) == Some("close")).then_some(node)
        })
        .unwrap();
    let rect = document.as_ref().get_client_bounding_rect(button).unwrap();
    let hit = pointer_hit(
        document.as_ref(),
        (rect.x + rect.width / 2.0) as f32,
        (rect.y + rect.height / 2.0) as f32,
    )
    .unwrap();
    assert!(
        std::iter::successors(Some(hit), |node| document
            .as_ref()
            .get_node(*node)
            .and_then(|node| node.parent))
        .any(|node| node == button),
        "the dialog button was not in the hit target's event path"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn native_pointer_sequence_dispatches_a_delegated_click() {
    tokio::task::LocalSet::new()
            .run_until(async {
                let html = r#"
                    <style>
                      :root, body { width: 100%; height: 100%; margin: 0; }
                      button { position: absolute; left: 10px; top: 10px; width: 120px; height: 50px; }
                    </style>
                    <button id="action">Action</button>
                    <script>
                      document.addEventListener("click", event => {
                        let target = event.target;
                        while (target && target.id !== "action") target = target.parentNode;
                        target?.setAttribute("data-clicked", "true");
                      });
                    </script>
                "#;
                let mut document =
                    blitz_html::HtmlDocument::from_html(html, blitz_dom::DocumentConfig::default());
                document
                    .as_mut()
                    .set_viewport(blitz_traits::shell::Viewport::new(
                        800,
                        600,
                        1.0,
                        blitz_traits::shell::ColorScheme::Dark,
                    ));
                document.as_mut().resolve(0.0);
                let script_to_dom = document
                    .as_ref()
                    .tree()
                    .iter()
                    .map(|(id, _)| (id, id))
                    .collect::<HashMap<_, _>>();
                let dom_to_script = script_to_dom.clone();
                let (_, arrivals) = channel();
                let mut layer = ShellDocument {
                    document,
                    arrivals,
                    bands: vec![Scene::new()],
                    placements: Vec::new(),
                    width: 800,
                    height: 600,
                    scale_factor: 1.0,
                    dirty: true,
                    needs_resolve: true,
                    needs_geometry_sync: true,
                    animation_epoch: std::time::Instant::now(),
                    script_to_dom,
                    dom_to_script,
                    pointer_hover_path: Vec::new(),
                };
                let (mut runtime, _notifications) = ShellRuntime::start(
                    layer.document.as_ref(),
                    800,
                    600,
                    1.0,
                    crate::shell_runtime::ShellRuntimeOptions {
                        html: html.to_owned(),
                        source_name: "snowglass:///native-click-test.html".to_owned(),
                        emulator_commands: deno::EmulatorCommandSink::default(),
                        input_commands: deno::InputCommandSink::default(),
                        webrtc_commands: deno::WebRtcCommandSink::default(),
                        startup_request: None,
                        plugin_runtime: Arc::new(snowglass_pluginhost::PluginRuntime::new(
                            snowglass_pluginhost::PluginRegistry::new(),
                            snowglass_pluginhost::PluginRuntimeOptions::default(),
                        )),
                        viewer: None,
                    },
                )
                .await
                .unwrap();
                layer.apply_script_mutations(&mut runtime);
                let hit = layer.hit_test(30.0, 30.0).unwrap();
                let event = |kind, buttons| DomMouseEvent {
                    kind,
                    x: 30.0,
                    y: 30.0,
                    button: 0,
                    buttons,
                    ctrl_key: false,
                    shift_key: false,
                };
                layer.mouse_event(&mut runtime, hit, event("mousedown", 1));
                layer.mouse_event(&mut runtime, hit, event("mouseup", 0));

                let clicked = layer.document.as_ref().tree().iter().any(|(_, node)| {
                    node.element_data().is_some_and(|element| {
                        element.attr(local_name!("id")) == Some("action")
                            && element.attr(LocalName::from("data-clicked")) == Some("true")
                    })
                });
                assert!(clicked, "the generated click did not reach the delegated listener");
            })
            .await;
}

#[test]
fn styles_new_solid_subtrees_before_attribute_invalidation() {
    let html = r#"
            <style>
              .stage { display: flex; width: 400px; height: 200px; }
              .child { flex: 1; }
            </style>
            <div id="root"></div>
        "#;
    let mut document =
        blitz_html::HtmlDocument::from_html(html, blitz_dom::DocumentConfig::default());
    document
        .as_mut()
        .set_viewport(blitz_traits::shell::Viewport::new(
            800,
            600,
            1.0,
            blitz_traits::shell::ColorScheme::Dark,
        ));
    document.as_mut().resolve(1.0);
    let root = document
        .as_ref()
        .tree()
        .iter()
        .find_map(|(id, node)| (node.attr(local_name!("id")) == Some("root")).then_some(id))
        .unwrap();
    let mut script_to_dom = document
        .as_ref()
        .tree()
        .iter()
        .map(|(id, _)| (id, id))
        .collect::<HashMap<_, _>>();
    let mut dom_to_script = script_to_dom.clone();
    let stage = script_to_dom.keys().copied().max().unwrap() + 1;
    let child = stage + 1;

    apply_dom_mutations(
        &mut document,
        &mut script_to_dom,
        &mut dom_to_script,
        vec![
            deno::Mutation::CreateElement {
                node: stage,
                tag: "section".to_owned(),
                namespace: None,
            },
            deno::Mutation::CreateElement {
                node: child,
                tag: "div".to_owned(),
                namespace: None,
            },
            deno::Mutation::AppendChild {
                parent: stage,
                child,
            },
            deno::Mutation::AppendChild {
                parent: root,
                child: stage,
            },
            deno::Mutation::SetAttribute {
                node: stage,
                name: "class".to_owned(),
                value: "stage".to_owned(),
            },
            deno::Mutation::SetAttribute {
                node: child,
                name: "class".to_owned(),
                value: "child".to_owned(),
            },
        ],
    );
    document.as_mut().resolve(1.0);

    let stage = script_to_dom[&stage];
    assert_eq!(
        document
            .as_ref()
            .tree()
            .get(stage)
            .unwrap()
            .attr(local_name!("class")),
        Some("stage")
    );
}

#[tokio::test]
async fn dynamically_inserted_tracked_surfaces_are_placed() {
    let html = r##"
            <style>
              :root, body, #root { width: 100%; height: 100%; margin: 0; }
              .stage { position: absolute; inset: 0; }
              .primary { position: absolute; inset: 16px; }
              .secondary {
                position: absolute;
                right: 24px;
                bottom: 24px;
                width: 200px;
                height: 100px;
              }
              .surface { position: absolute; inset: 0; width: 100%; height: 100%; }
            </style>
            <div id="root"></div>
            <script>
              const stage = document.createElement("section");
              stage.className = "stage";
              for (const [index, className] of [[0, "primary"], [1, "secondary"]]) {
                const frame = document.createElement("div");
                frame.className = className;
                const surface = document.createElement("tracked-surface");
                surface.className = "surface";
                surface.setAttribute("src", String(index));
                frame.appendChild(surface);
                stage.appendChild(frame);
              }
              document.querySelector("#root").appendChild(stage);
            </script>
        "##;
    let mut document =
        blitz_html::HtmlDocument::from_html(html, blitz_dom::DocumentConfig::default());
    document
        .as_mut()
        .set_viewport(blitz_traits::shell::Viewport::new(
            800,
            600,
            1.0,
            blitz_traits::shell::ColorScheme::Dark,
        ));
    document.as_mut().resolve(1.0);
    let mut script_to_dom = document
        .as_ref()
        .tree()
        .iter()
        .map(|(id, _)| (id, id))
        .collect::<HashMap<_, _>>();
    let mut dom_to_script = script_to_dom.clone();
    let mut runtime = deno::Runtime::new(
        document.as_ref(),
        html,
        "snowglass:///test.html",
        800,
        600,
        deno::EmulatorCommandSink::default(),
    )
    .await
    .unwrap();
    apply_dom_mutations(
        &mut document,
        &mut script_to_dom,
        &mut dom_to_script,
        runtime.take_mutations(),
    );
    document.as_mut().resolve(1.0);

    let mut scenes = (0..3).map(|_| Scene::new()).collect::<Vec<_>>();
    let mut placements = Vec::new();
    let mut scene = BandedScene::new(&mut scenes, &mut placements);
    blitz_paint::paint_scene(&mut scene, document.as_mut(), 1.0, 800, 600, 0, 0);
    assert_eq!(
        placements
            .iter()
            .map(|placement| placement.source_id)
            .collect::<Vec<_>>(),
        [0, 1]
    );
    assert_eq!(placements[0].rect.width(), 768.0);
    assert_eq!(placements[0].rect.height(), 568.0);
    assert_eq!(placements[1].rect.width(), 200.0);
    assert_eq!(placements[1].rect.height(), 100.0);
}
