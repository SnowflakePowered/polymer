use super::*;

fn arguments(values: &[&str]) -> Result<ViewerOptions, Box<dyn Error>> {
    parse_arguments(values.iter().map(|value| (*value).to_owned()))
}

fn tracked(index: u32, quarter_turns: u8) -> TrackedRenderTarget {
    TrackedRenderTarget {
        index,
        quarter_turns,
        surface_pattern: None,
    }
}

#[test]
fn defaults_to_d3d12() -> Result<(), Box<dyn Error>> {
    let options = arguments(&["--swapchain-resolution", "640", "480", "--", "game.exe"])?;
    assert_eq!(options.api, ViewerApi::D3d12);
    assert_eq!(options.track_render_targets, None);
    assert_eq!(options.layout_css, None);
    assert_eq!(options.surfaces, 16);
    assert_eq!(options.skip_surfaces, 0);
    assert!(options.isolate);
    assert_eq!(options.hooks, AUTO_HOOKS);
    assert_eq!(options.target, ["game.exe"]);
    assert_eq!(options.webrtc, None);
    assert_eq!(options.bind_webrtc, "127.0.0.1:8080".parse()?);
    Ok(())
}

#[test]
fn accepts_render_target_layout_css() -> Result<(), Box<dyn Error>> {
    let options = arguments(&[
        "--swapchain-resolution",
        "640",
        "480",
        "--layout-css",
        "display: grid; grid-template-columns: 1fr 2fr",
        "--",
        "game.exe",
    ])?;
    assert_eq!(
        options.layout_css.as_deref(),
        Some("display: grid; grid-template-columns: 1fr 2fr")
    );
    Ok(())
}

#[test]
fn cli_target_arguments_are_not_reparsed_or_requoted() -> Result<(), Box<dyn Error>> {
    let options = arguments(&[
        "--swapchain-resolution",
        "640",
        "480",
        "--track-render-targets",
        "0",
        "--hook",
        "d3d11",
        "--no-isolate",
        "--",
        r#"..\Dolphin-ARM64\Dolphin.exe"#,
        "-v",
        "D3D11",
        r#"..\Wii Sports (USA) (Rev 1).rvz"#,
    ])?;
    let expected = options.target.clone();
    assert_eq!(LauncherState::from_options(&options).target()?, expected);
    assert_eq!(expected[3], r#"..\Wii Sports (USA) (Rev 1).rvz"#);
    Ok(())
}

#[test]
fn accepts_no_target_for_interactive_launcher() -> Result<(), Box<dyn Error>> {
    let options = arguments(&[])?;
    assert!(options.target.is_empty());
    assert_eq!(options.api, ViewerApi::D3d12);
    assert_eq!(options.hooks, AUTO_HOOKS);
    Ok(())
}

#[test]
fn launcher_splits_quoted_windows_arguments() {
    assert_eq!(
        split_command_arguments(r#"--profile "C:\Users\Player One\game.ini" --fullscreen"#)
            .unwrap(),
        [
            "--profile",
            r#"C:\Users\Player One\game.ini"#,
            "--fullscreen"
        ]
    );
    assert!(split_command_arguments("\"unfinished").is_err());
}

#[test]
fn launcher_supports_sparse_virtual_slot_indices() -> Result<(), Box<dyn Error>> {
    let options = arguments(&[])?;
    let mut launcher = LauncherState::from_options(&options);
    launcher.gamepads[0].slot = 3;
    let pads = launcher.configured_gamepads()?;
    assert_eq!(pads.len(), 4);
    assert_eq!(pads[3].slot, 3);
    assert_eq!(pads[0].source, None);
    Ok(())
}

#[test]
fn launcher_preserves_render_target_order_and_rotation() -> Result<(), Box<dyn Error>> {
    let options = arguments(&["--hook", "opengl", "--track-render-targets=4:1,2:3"])?;
    let launcher = LauncherState::from_options(&options);
    assert_eq!(launcher.tracked_render_target_indices(), Some(vec![4, 2]));
    assert_eq!(launcher.render_target_rotations(), [1, 3]);
    assert!(launcher.validate_render_targets().is_ok());
    Ok(())
}

#[test]
fn hook_selection_defaults_to_auto() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        arguments(&[
            "--swapchain-resolution",
            "640",
            "480",
            "--hook",
            "auto",
            "--",
            "game.exe"
        ])?
        .hooks,
        AUTO_HOOKS
    );
    Ok(())
}

#[test]
fn hook_order_defines_priority() -> Result<(), Box<dyn Error>> {
    let options = arguments(&[
        "--swapchain-resolution",
        "640",
        "480",
        "--hook",
        "d3d12,d3d11",
        "--hook",
        "vulkan",
        "--",
        "game.exe",
    ])?;
    assert_eq!(
        options.hooks,
        [
            GraphicsHook::D3d12,
            GraphicsHook::D3d11,
            GraphicsHook::Vulkan
        ]
    );
    Ok(())
}

#[test]
fn hook_auto_appends_the_remaining_backends() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        arguments(&[
            "--swapchain-resolution",
            "640",
            "480",
            "--hook=d3d11,auto",
            "--",
            "game.exe"
        ])?
        .hooks,
        [
            GraphicsHook::D3d11,
            GraphicsHook::Vulkan,
            GraphicsHook::D3d12,
        ]
    );
    Ok(())
}

#[test]
fn rejects_unknown_and_repeated_hooks() {
    assert!(
        arguments(&[
            "--swapchain-resolution",
            "640",
            "480",
            "--hook",
            "metal",
            "--",
            "game.exe"
        ])
        .is_err()
    );
    assert!(
        arguments(&[
            "--swapchain-resolution",
            "640",
            "480",
            "--hook",
            "vulkan,vulkan",
            "--",
            "game.exe"
        ])
        .is_err()
    );
}

#[test]
fn accepts_vulkan_override_and_options_in_either_order() -> Result<(), Box<dyn Error>> {
    let options = arguments(&[
        "--hook",
        "opengl",
        "--swapchain-resolution",
        "640",
        "480",
        "--track-render-target",
        "--no-isolate",
        "--api=vulkan",
        "--",
        "game.exe",
        "--api",
        "target-argument",
    ])?;
    assert_eq!(options.api, ViewerApi::Vulkan);
    assert_eq!(options.track_render_targets, Some(vec![tracked(0, 0)]));
    assert!(!options.isolate);
    assert_eq!(options.target, ["game.exe", "--api", "target-argument"]);
    Ok(())
}

#[test]
fn resize_is_available_only_with_isolation() -> Result<(), Box<dyn Error>> {
    assert!(
        arguments(&[
            "--swapchain-resolution",
            "640",
            "480",
            "--resize",
            "--",
            "game.exe"
        ])?
        .resize
    );
    assert!(
        arguments(&[
            "--swapchain-resolution",
            "640",
            "480",
            "--resize",
            "--no-isolate",
            "--",
            "game.exe"
        ])
        .is_err()
    );
    Ok(())
}

#[test]
fn accepts_render_target_indices_and_defaults_to_zero() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        arguments(&[
            "--swapchain-resolution",
            "640",
            "480",
            "--track-render-targets",
            "0,1",
            "--",
            "game.exe"
        ])?
        .track_render_targets,
        Some(vec![tracked(0, 0), tracked(1, 0)])
    );
    assert_eq!(
        arguments(&[
            "--swapchain-resolution",
            "640",
            "480",
            "--track-render-targets",
            "--",
            "game.exe"
        ])?
        .track_render_targets,
        Some(vec![tracked(0, 0)])
    );
    assert_eq!(
        arguments(&[
            "--swapchain-resolution",
            "640",
            "480",
            "--track-render-targets=1",
            "--",
            "game.exe"
        ])?
        .track_render_targets,
        Some(vec![tracked(1, 0)])
    );
    Ok(())
}

#[test]
fn accepts_render_target_rotations_in_quarter_turns() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        arguments(&[
            "--swapchain-resolution",
            "640",
            "480",
            "--track-render-targets",
            "0:1,1",
            "--",
            "game.exe"
        ])?
        .track_render_targets,
        Some(vec![tracked(0, 1), tracked(1, 0)])
    );
    // Quarter turns wrap around the unit circle.
    assert_eq!(
        arguments(&[
            "--swapchain-resolution",
            "640",
            "480",
            "--track-render-targets=0:4,1:7",
            "--",
            "game.exe"
        ])?
        .track_render_targets,
        Some(vec![tracked(0, 0), tracked(1, 3)])
    );
    Ok(())
}

#[test]
fn accepts_render_target_surface_regexes() -> Result<(), Box<dyn Error>> {
    let options = arguments(&[
        "--swapchain-resolution",
        "640",
        "480",
        "--track-render-targets",
        "0:0:(?i)gamepad view,1:3:Main: [0-9]+",
        "--",
        "game.exe",
    ])?;
    let targets = options.track_render_targets.unwrap();
    assert_eq!(targets[0].index, 0);
    assert_eq!(targets[0].quarter_turns, 0);
    assert_eq!(
        targets[0].surface_pattern.as_deref(),
        Some("(?i)gamepad view")
    );
    assert_eq!(targets[1].index, 1);
    assert_eq!(targets[1].quarter_turns, 3);
    assert_eq!(targets[1].surface_pattern.as_deref(), Some("Main: [0-9]+"));
    assert!(
        arguments(&[
            "--swapchain-resolution",
            "640",
            "480",
            "--track-render-targets",
            "0:0:(",
            "--",
            "game.exe"
        ])
        .is_err()
    );
    Ok(())
}

#[test]
fn rejects_invalid_render_target_indices() {
    assert!(
        arguments(&[
            "--swapchain-resolution",
            "640",
            "480",
            "--track-render-targets",
            "16",
            "--",
            "game.exe"
        ])
        .is_err()
    );
    assert!(
        arguments(&[
            "--swapchain-resolution",
            "640",
            "480",
            "--track-render-targets=0,0",
            "--",
            "game.exe"
        ])
        .is_err()
    );
    assert!(
        arguments(&[
            "--swapchain-resolution",
            "640",
            "480",
            "--track-render-targets=0:x",
            "--",
            "game.exe"
        ])
        .is_err()
    );
    assert!(
        arguments(&[
            "--swapchain-resolution",
            "640",
            "480",
            "--track-render-targets=0:",
            "--",
            "game.exe"
        ])
        .is_err()
    );
    // Rotation does not distinguish otherwise duplicate indices.
    assert!(
        arguments(&[
            "--swapchain-resolution",
            "640",
            "480",
            "--track-render-targets=0:1,0:2",
            "--",
            "game.exe"
        ])
        .is_err()
    );
}

#[test]
fn accepts_webrtc_defaults_and_target_indices() -> Result<(), Box<dyn Error>> {
    let defaults = arguments(&[
        "--swapchain-resolution",
        "640",
        "480",
        "--webrtc",
        "--",
        "game.exe",
    ])?;
    assert_eq!(defaults.webrtc, Some(vec![0]));
    let explicit = arguments(&[
        "--swapchain-resolution",
        "640",
        "480",
        "--webrtc=0,1",
        "--bind-webrtc",
        "0.0.0.0:9000",
        "--",
        "game.exe",
    ])?;
    assert_eq!(explicit.webrtc, Some(vec![0, 1]));
    assert_eq!(explicit.bind_webrtc, "0.0.0.0:9000".parse()?);
    Ok(())
}

#[test]
fn rejects_invalid_webrtc_options() {
    assert!(
        arguments(&[
            "--swapchain-resolution",
            "640",
            "480",
            "--webrtc=16",
            "--",
            "game.exe"
        ])
        .is_err()
    );
    assert!(
        arguments(&[
            "--swapchain-resolution",
            "640",
            "480",
            "--webrtc=0,0",
            "--",
            "game.exe"
        ])
        .is_err()
    );
    assert!(
        arguments(&[
            "--swapchain-resolution",
            "640",
            "480",
            "--bind-webrtc=0.0.0.0:9000",
            "--",
            "game.exe"
        ])
        .is_err()
    );
}

#[test]
fn accepts_a_surface_count_and_defaults_to_one() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        arguments(&[
            "--swapchain-resolution",
            "640",
            "480",
            "--surfaces",
            "2",
            "--",
            "game.exe"
        ])?
        .surfaces,
        2
    );
    assert_eq!(
        arguments(&[
            "--swapchain-resolution",
            "640",
            "480",
            "--surfaces",
            "--",
            "game.exe"
        ])?
        .surfaces,
        16
    );
    assert_eq!(
        arguments(&[
            "--swapchain-resolution",
            "640",
            "480",
            "--surfaces=3",
            "--",
            "game.exe"
        ])?
        .surfaces,
        3
    );
    Ok(())
}

#[test]
fn rejects_an_invalid_surface_count() {
    assert!(
        arguments(&[
            "--swapchain-resolution",
            "640",
            "480",
            "--surfaces",
            "0",
            "--",
            "game.exe"
        ])
        .is_err()
    );
    assert!(
        arguments(&[
            "--swapchain-resolution",
            "640",
            "480",
            "--surfaces=17",
            "--",
            "game.exe"
        ])
        .is_err()
    );
}

#[test]
fn accepts_a_surface_skip_count_and_defaults_to_one() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        arguments(&[
            "--swapchain-resolution",
            "640",
            "480",
            "--skip-surfaces",
            "2",
            "--",
            "game.exe"
        ])?
        .skip_surfaces,
        2
    );
    assert_eq!(
        arguments(&[
            "--swapchain-resolution",
            "640",
            "480",
            "--skip-surfaces",
            "--",
            "game.exe"
        ])?
        .skip_surfaces,
        1
    );
    assert_eq!(
        arguments(&[
            "--swapchain-resolution",
            "640",
            "480",
            "--skip-windows=3",
            "--",
            "game.exe"
        ])?
        .skip_surfaces,
        3
    );
    Ok(())
}

#[test]
fn rejects_an_invalid_surface_skip_count() {
    assert!(
        arguments(&[
            "--swapchain-resolution",
            "640",
            "480",
            "--skip-surfaces=menu",
            "--",
            "game.exe"
        ])
        .is_err()
    );
}

fn offset_tuples(offsets: [ash::vk::Offset3D; 2]) -> [(i32, i32, i32); 2] {
    offsets.map(|offset| (offset.x, offset.y, offset.z))
}

#[test]
fn bottom_up_streams_reverse_vulkan_source_y() {
    assert_eq!(
        offset_tuples(polymer_renderer::renderer::vulkan::blit_source_offsets(
            1280, 720, true, 0,
        )),
        [(0, 720, 0), (1280, 0, 1)]
    );
    assert_eq!(
        offset_tuples(polymer_renderer::renderer::vulkan::blit_source_offsets(
            1280, 720, false, 0,
        )),
        [(0, 0, 0), (1280, 720, 1)]
    );
    assert_eq!(polymer_renderer::vertical_layout_index(0, 2, true), 1);
    assert_eq!(polymer_renderer::vertical_layout_index(1, 2, true), 0);
    assert_eq!(polymer_renderer::vertical_layout_index(0, 2, false), 0);
}

#[test]
fn half_turns_mirror_vulkan_blit_source_offsets() {
    assert_eq!(
        offset_tuples(polymer_renderer::renderer::vulkan::blit_source_offsets(
            1280, 720, false, 2,
        )),
        [(1280, 720, 0), (0, 0, 1)]
    );
    // A flipped half turn cancels the vertical mirror.
    assert_eq!(
        offset_tuples(polymer_renderer::renderer::vulkan::blit_source_offsets(
            1280, 720, true, 2,
        )),
        [(1280, 0, 0), (0, 720, 1)]
    );
}

#[test]
fn virtual_swapchain_cli_preserves_extent_and_rejects_conflicting_modes() {
    let options = arguments(&["--swapchain-resolution", "640", "528", "--hook", "d3d11"]).unwrap();
    assert_eq!(
        LauncherState::from_options(&options).swapchain_resolution,
        Some([640, 528])
    );
    for args in [
        vec!["--swapchain-resolution", "0", "528"],
        vec!["--swapchain-resolution", "640"],
        vec!["--swapchain-resolution", "2147483648", "528"],
        vec!["--swapchain-resolution", "640", "528", "--force-km"],
        vec![
            "--swapchain-resolution",
            "640",
            "528",
            "--track-render-targets",
            "0",
            "--graph-tracking",
        ],
    ] {
        assert!(arguments(&args).is_err(), "accepted {args:?}");
    }
}

#[test]
fn production_apis_reject_capture_options_and_require_fixed_extent() {
    for hook in ["d3d11", "d3d12", "vulkan", "opengl"] {
        assert!(arguments(&["--hook", hook, "--", "game.exe"]).is_err());
        assert!(
            arguments(&[
                "--hook",
                hook,
                "--track-render-targets=0",
                "--graph-tracking"
            ])
            .is_err()
        );
        assert!(arguments(&["--hook", hook, "--force-km"]).is_err());
        assert!(
            arguments(&[
                "--hook",
                hook,
                "--swapchain-resolution",
                "640",
                "480",
                "--",
                "game.exe"
            ])
            .is_ok()
        );
    }
    assert!(
        arguments(&[
            "--hook=opengl,vulkan",
            "--swapchain-resolution",
            "640",
            "480"
        ])
        .is_ok()
    );
    assert!(
        arguments(&[
            "--hook=opengl",
            "--track-render-targets=0",
            "--",
            "game.exe"
        ])
        .is_err()
    );
}
