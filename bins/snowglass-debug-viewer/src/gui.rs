use super::*;
use consolespec::input::BindingKey;

#[derive(Clone)]
pub(crate) struct LauncherGamepad {
    pub(crate) slot: usize,
    pub(crate) selector: String,
    pub(crate) layout: String,
    pub(crate) force_mouse: bool,
}

pub(crate) struct LauncherState {
    cli_target: Option<Vec<String>>,
    pub(crate) executable: String,
    pub(crate) arguments: String,
    pub(crate) isolate: bool,
    pub(crate) hooks: Vec<HookBackend>,
    pub(crate) render_targets: Vec<TrackedRenderTarget>,
    pub(crate) graph_tracking: bool,
    pub(crate) gamepads: Vec<LauncherGamepad>,
}

pub(crate) fn draw_layout_css_window(
    context: &egui::Context,
    open: &mut bool,
    css: &mut String,
    error: &mut Option<String>,
) {
    egui::Window::new("Render target CSS")
        .open(open)
        .default_pos(egui::pos2(24.0, 72.0))
        .default_size(egui::vec2(480.0, 360.0))
        .resizable(true)
        .show(context, |ui| {
            ui.label("Selectors: #viewer, .render-target, #render-target-N");
            ui.add_space(4.0);
            let changed = ui
                .add(
                    egui::TextEdit::multiline(css)
                        .code_editor()
                        .desired_rows(14)
                        .desired_width(f32::INFINITY),
                )
                .changed();
            if changed {
                if css.trim().is_empty() {
                    layout::DEFAULT_CSS.clone_into(css);
                }
                let result = layout::configure(Some(css));
                *error = result.err();
            }
            ui.horizontal(|ui| {
                if ui.button("Reset to default").clicked() {
                    layout::DEFAULT_CSS.clone_into(css);
                    *error = layout::configure(Some(css)).err();
                }
                if error.is_none() {
                    ui.colored_label(egui::Color32::LIGHT_GREEN, "Applied");
                }
            });
            if let Some(message) = error {
                ui.colored_label(egui::Color32::LIGHT_RED, message);
                ui.label("The last valid layout remains active.");
            }
        });
}

impl LauncherState {
    pub(crate) fn from_options(options: &ViewerOptions) -> Self {
        Self {
            cli_target: (!options.target.is_empty()).then(|| options.target.clone()),
            executable: options.target.first().cloned().unwrap_or_default(),
            arguments: options
                .target
                .iter()
                .skip(1)
                .cloned()
                .collect::<Vec<_>>()
                .join(" "),
            isolate: options.isolate,
            hooks: options.hooks.clone(),
            render_targets: options.track_render_targets.clone().unwrap_or_default(),
            graph_tracking: options.graph_tracking,
            gamepads: options
                .gamepads
                .iter()
                .map(|pad| LauncherGamepad {
                    slot: pad.slot,
                    selector: match &pad.source {
                        Some(SourceSelector::KeyboardMouse) => "K".to_owned(),
                        Some(SourceSelector::Device { device_id }) => device_id.clone(),
                        None => String::new(),
                    },
                    layout: pad.layout.clone(),
                    force_mouse: pad.force_mouse,
                })
                .collect(),
        }
    }

    pub(crate) fn target(&self) -> Result<Vec<String>, String> {
        if let Some(target) = &self.cli_target {
            return Ok(target.clone());
        }
        if self.executable.trim().is_empty() {
            return Err("Choose an executable first".to_owned());
        }
        let mut target = vec![self.executable.trim().to_owned()];
        target.extend(split_command_arguments(&self.arguments)?);
        Ok(target)
    }

    pub(crate) fn gamepad_arguments(&self) -> Result<Vec<GamepadArgument>, String> {
        let mut pads = self
            .gamepads
            .iter()
            .map(|pad| {
                format!(
                    "{}{}:{}:{}",
                    pad.selector.trim(),
                    if pad.force_mouse { "+M" } else { "" },
                    pad.slot,
                    pad.layout
                )
                .parse::<GamepadArgument>()
            })
            .collect::<Result<Vec<_>, _>>()?;
        pads.sort_by_key(|pad| pad.slot);
        if pads.windows(2).any(|pads| pads[0].slot == pads[1].slot) {
            return Err("Virtual controller slot indices must be unique".to_owned());
        }
        if let Some(last_slot) = pads.last().map(|pad| pad.slot) {
            let mut by_slot = pads
                .into_iter()
                .map(|pad| (pad.slot, pad))
                .collect::<HashMap<_, _>>();
            return Ok((0..=last_slot)
                .map(|slot| {
                    by_slot.remove(&slot).unwrap_or(GamepadArgument {
                        source: None,
                        slot,
                        layout: "XBOX_CONTROLLER".to_owned(),
                        force_mouse: false,
                    })
                })
                .collect());
        }
        Err("Add at least one virtual controller slot".to_owned())
    }

    pub(crate) fn tracked_render_target_indices(&self) -> Option<Vec<u32>> {
        (!self.render_targets.is_empty()).then(|| {
            self.render_targets
                .iter()
                .map(|target| target.index)
                .collect()
        })
    }

    pub(crate) fn render_target_rotations(&self) -> Vec<u8> {
        self.render_targets
            .iter()
            .map(|target| target.quarter_turns)
            .collect()
    }

    pub(crate) fn render_target_surface_patterns(&self) -> Vec<Option<String>> {
        self.render_targets
            .iter()
            .map(|target| target.surface_pattern.clone())
            .collect()
    }

    pub(crate) fn validate_render_targets(&self) -> Result<(), String> {
        let mut indices = HashSet::new();
        if self
            .render_targets
            .iter()
            .any(|target| target.index >= 16 || !indices.insert(target.index))
        {
            return Err(
                "Tracked render-target indices must be unique values from 0 through 15".to_owned(),
            );
        }
        for target in &self.render_targets {
            if let Some(pattern) = &target.surface_pattern {
                regex::Regex::new(pattern)
                    .map_err(|error| format!("Invalid surface regex '{pattern}': {error}"))?;
            }
        }
        Ok(())
    }
}
pub(crate) fn draw_gamepad_window(
    context: &egui::Context,
    provider: &Sdl3Provider,
    layouts: &[InputLayout],
    drafts: &mut [HashMap<BindingKey, String>],
    latency: &snowglass_statistics::DistributionStatistics,
) {
    const BUTTONS: [SdlButton; 21] = [
        SdlButton::South,
        SdlButton::East,
        SdlButton::West,
        SdlButton::North,
        SdlButton::Back,
        SdlButton::Guide,
        SdlButton::Start,
        SdlButton::LeftStick,
        SdlButton::RightStick,
        SdlButton::LeftShoulder,
        SdlButton::RightShoulder,
        SdlButton::DpadUp,
        SdlButton::DpadDown,
        SdlButton::DpadLeft,
        SdlButton::DpadRight,
        SdlButton::Misc1,
        SdlButton::RightPaddle1,
        SdlButton::LeftPaddle1,
        SdlButton::RightPaddle2,
        SdlButton::LeftPaddle2,
        SdlButton::Touchpad,
    ];
    const AXES: [SdlAxis; 6] = [
        SdlAxis::LeftX,
        SdlAxis::LeftY,
        SdlAxis::RightX,
        SdlAxis::RightY,
        SdlAxis::LeftTrigger,
        SdlAxis::RightTrigger,
    ];

    let states = provider.states();
    egui::Window::new("Gamepads")
        .default_pos(egui::pos2(880.0, 48.0))
        .default_width(380.0)
        .show(context, |ui| {
            if latency.samples == 0 {
                ui.colored_label(egui::Color32::GRAY, "Input dispatch latency: waiting");
            } else {
                ui.label(format!(
                    "Input dispatch latency  p50 {:.3} ms  p95 {:.3} ms  p99 {:.3} ms  (n={})",
                    latency.p50, latency.p95, latency.p99, latency.samples
                ))
                .on_hover_text(
                    "Physical SDL sample to completion of the AF_UNIX send; excludes Dolphin's subsequent SDL update.",
                );
            }
            ui.separator();
            for (slot, layout) in layouts.iter().enumerate() {
                egui::CollapsingHeader::new(format!("Slot {slot}: {}", layout.name))
                    .default_open(slot == 0)
                    .show(ui, |ui| {
                        let Some(state) = states.get(slot).and_then(Option::as_ref) else {
                            ui.colored_label(egui::Color32::GRAY, "Controller disconnected");
                            return;
                        };
                        ui.strong(&state.name);
                        ui.monospace(&state.guid);
                        ui.label(format!(
                            "Sticks L {:?}  R {:?}   Triggers {} / {}",
                            state.mapped.left_stick,
                            state.mapped.right_stick,
                            state.mapped.left_trigger,
                            state.mapped.right_trigger
                        ));
                        let pressed = state
                            .buttons
                            .iter()
                            .filter_map(|(button, pressed)| pressed.then(|| format!("{button:?}")))
                            .collect::<Vec<_>>();
                        ui.label(if pressed.is_empty() {
                            "Pressed: none".to_owned()
                        } else {
                            format!("Pressed: {}", pressed.join(", "))
                        });
                        ui.label(format!(
                            "Axes: {}",
                            state
                                .axes
                                .iter()
                                .map(|(axis, value)| format!("{axis:?}={value}"))
                                .collect::<Vec<_>>()
                                .join("  ")
                        ));
                        if state.mapped.accelerometer != [0.0; 3]
                            || state.mapped.gyroscope != [0.0; 3]
                        {
                            ui.label(format!(
                                "Sensors: accel {:?} m/s²   gyro {:?} rad/s",
                                state.mapped.accelerometer, state.mapped.gyroscope
                            ));
                        }
                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.strong("Real SDL input → element");
                            if ui.small_button("Reset").clicked() {
                                drafts[slot].clear();
                                provider.reset_bindings(slot);
                            }
                        });
                        egui::ScrollArea::vertical()
                            .max_height(300.0)
                            .show(ui, |ui| {
                                for element in &layout.elements {
                                    if !is_remappable_element(element.key) {
                                        continue;
                                    }
                                    let selected = drafts[slot]
                                        .entry(element.key)
                                        .or_insert_with(|| "Default".to_owned());
                                    let previous = selected.clone();
                                    ui.horizontal(|ui| {
                                        ui.label(&element.label).on_hover_text(element.key.to_string());
                                        egui::ComboBox::from_id_salt((
                                            "gamepad-remap",
                                            slot,
                                            &element.id,
                                        ))
                                        .selected_text(selected.as_str())
                                        .width(145.0)
                                        .show_ui(
                                            ui,
                                            |ui| {
                                                ui.selectable_value(
                                                    selected,
                                                    "Default".to_owned(),
                                                    "Default",
                                                );
                                                ui.selectable_value(
                                                    selected,
                                                    "Disabled".to_owned(),
                                                    "Disabled",
                                                );
                                                for button in BUTTONS {
                                                    let value = format!("Button:{button:?}");
                                                    ui.selectable_value(
                                                        selected,
                                                        value.clone(),
                                                        value,
                                                    );
                                                }
                                                if element_accepts_axis(element.key) {
                                                    for axis in AXES {
                                                        let value = format!("Axis:{axis:?}");
                                                        ui.selectable_value(
                                                            selected,
                                                            value.clone(),
                                                            value,
                                                        );
                                                    }
                                                }
                                            },
                                        );
                                    });
                                    if *selected != previous {
                                        if let Some(binding) = ui_binding(selected) {
                                            provider.set_binding(slot, element.key, binding);
                                        } else {
                                            provider.clear_binding(slot, element.key);
                                        }
                                    }
                                }
                            });
                    });
            }
        });
}

pub(crate) fn draw_launcher_window(
    context: &egui::Context,
    launcher: &mut LauncherState,
    error: &mut Option<String>,
) -> bool {
    let mut start = false;
    egui::Window::new("Start a program")
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .default_width(620.0)
        .collapsible(false)
        .resizable(true)
        .show(context, |ui| {
            ui.label("Executable");
            if ui.text_edit_singleline(&mut launcher.executable).changed() {
                launcher.cli_target = None;
            }
            ui.label("Arguments");
            if ui.text_edit_singleline(&mut launcher.arguments).changed() {
                launcher.cli_target = None;
            }
            ui.checkbox(&mut launcher.isolate, "Run on an isolated desktop");
            ui.separator();
            ui.strong("Graphics hooks");
            ui.horizontal_wrapped(|ui| {
                for hook in HookBackend::ALL {
                    let mut enabled = launcher.hooks.contains(&hook);
                    if ui.checkbox(&mut enabled, hook.label()).changed() {
                        if enabled {
                            launcher.hooks.push(hook);
                        } else {
                            launcher.hooks.retain(|value| *value != hook);
                        }
                    }
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.strong("Tracked render targets");
                if ui.small_button("Add target").clicked() {
                    let index = (0..16)
                        .find(|index| {
                            !launcher
                                .render_targets
                                .iter()
                                .any(|target| target.index == *index)
                        })
                        .unwrap_or(0);
                    launcher.render_targets.push(TrackedRenderTarget {
                        index,
                        quarter_turns: 0,
                        surface_pattern: None,
                    });
                }
            });
            ui.label("Rows are displayed in this order. Rotation is counterclockwise.");
            let mut remove_target = None;
            for (row, target) in launcher.render_targets.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(format!("Display {}", row));
                    ui.label("Target index");
                    ui.add(egui::DragValue::new(&mut target.index).range(0..=15));
                    egui::ComboBox::from_id_salt(("launcher-target-rotation", row))
                        .selected_text(format!("{}°", u16::from(target.quarter_turns) * 90))
                        .show_ui(ui, |ui| {
                            for turns in 0..4 {
                                ui.selectable_value(
                                    &mut target.quarter_turns,
                                    turns,
                                    format!("{}°", u16::from(turns) * 90),
                                );
                            }
                        });
                    ui.label("Window title regex");
                    ui.text_edit_singleline(target.surface_pattern.get_or_insert_with(String::new));
                    if target.surface_pattern.as_deref() == Some("") {
                        target.surface_pattern = None;
                    }
                    if ui.small_button("Remove").clicked() {
                        remove_target = Some(row);
                    }
                });
            }
            if let Some(row) = remove_target {
                launcher.render_targets.remove(row);
            }
            ui.add_enabled_ui(!launcher.render_targets.is_empty(), |ui| {
                ui.checkbox(
                    &mut launcher.graph_tracking,
                    "Resolve targets through the presentation dependency graph",
                );
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.strong("Virtual controller slots");
                if ui.small_button("Add slot").clicked() {
                    let slot = launcher
                        .gamepads
                        .iter()
                        .map(|pad| pad.slot)
                        .max()
                        .map_or(0, |slot| slot + 1);
                    launcher.gamepads.push(LauncherGamepad {
                        slot,
                        selector: "K".to_owned(),
                        layout: "XBOX_CONTROLLER".to_owned(),
                        force_mouse: false,
                    });
                }
            });
            let mut remove = None;
            let mut layout_ids = InputLayout::all()
                .iter()
                .map(|layout| layout.id.clone())
                .collect::<Vec<_>>();
            layout_ids.sort_unstable();
            for (row, pad) in launcher.gamepads.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.label("Virtual index");
                    ui.add(egui::DragValue::new(&mut pad.slot).range(0..=u8::MAX as usize));
                    ui.label("Source (K or sdl3:<id>)");
                    ui.text_edit_singleline(&mut pad.selector);
                    egui::ComboBox::from_id_salt(("launcher-layout", row))
                        .selected_text(&pad.layout)
                        .show_ui(ui, |ui| {
                            for layout in &layout_ids {
                                ui.selectable_value(&mut pad.layout, layout.clone(), layout);
                            }
                        });
                    ui.checkbox(&mut pad.force_mouse, "Mouse");
                    if ui.small_button("Remove").clicked() {
                        remove = Some(row);
                    }
                });
            }
            if let Some(row) = remove {
                launcher.gamepads.remove(row);
            }
            if let Some(message) = error.as_deref() {
                ui.separator();
                ui.colored_label(egui::Color32::LIGHT_RED, message);
            }
            ui.separator();
            start = ui
                .add_enabled(
                    !launcher.executable.trim().is_empty(),
                    egui::Button::new("Start"),
                )
                .clicked();
        });
    start
}

fn is_remappable_element(key: BindingKey) -> bool {
    matches!(key, BindingKey::Button(_) | BindingKey::Directional { .. })
        || element_accepts_axis(key)
}

fn element_accepts_axis(key: BindingKey) -> bool {
    key.is_analog()
}

fn ui_binding(value: &str) -> Option<InputBinding> {
    if value == "Default" {
        return None;
    }
    if value == "Disabled" {
        return Some(InputBinding::Disabled { disabled: true });
    }
    if let Some(name) = value.strip_prefix("Button:") {
        let button = [
            SdlButton::South,
            SdlButton::East,
            SdlButton::West,
            SdlButton::North,
            SdlButton::Back,
            SdlButton::Guide,
            SdlButton::Start,
            SdlButton::LeftStick,
            SdlButton::RightStick,
            SdlButton::LeftShoulder,
            SdlButton::RightShoulder,
            SdlButton::DpadUp,
            SdlButton::DpadDown,
            SdlButton::DpadLeft,
            SdlButton::DpadRight,
            SdlButton::Misc1,
            SdlButton::RightPaddle1,
            SdlButton::LeftPaddle1,
            SdlButton::RightPaddle2,
            SdlButton::LeftPaddle2,
            SdlButton::Touchpad,
        ]
        .into_iter()
        .find(|button| format!("{button:?}") == name)?;
        return Some(InputBinding::Button { button });
    }
    let name = value.strip_prefix("Axis:")?;
    let axis = [
        SdlAxis::LeftX,
        SdlAxis::LeftY,
        SdlAxis::RightX,
        SdlAxis::RightY,
        SdlAxis::LeftTrigger,
        SdlAxis::RightTrigger,
    ]
    .into_iter()
    .find(|axis| format!("{axis:?}") == name)?;
    Some(InputBinding::Axis {
        axis,
        invert: false,
    })
}
