//! SDL3 physical-gamepad capture normalized into Stone-labelled snapshots.

use crate::{
    gamepad::{GamepadButton, GamepadSlot, GamepadState, GamepadUpdate, LatestGamepadMailbox},
    stone::{
        EMBEDDED_GAMECONTROLLER_DB, EMBEDDED_STONE_JSON, InputBinding, RemapDatabase, RemapProfile,
        SdlAxis, SdlButton, StoneLayout, StoneRegistry,
    },
};
use sdl3::gamepad::{Axis, Button, ButtonLabel, Gamepad};
use sdl3::event::Event;
use sdl3::sensor::SensorType;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub struct ProviderConfig {
    pub slots: Vec<SlotConfig>,
    pub stone_database: Option<PathBuf>,
    pub gamecontroller_database: Option<PathBuf>,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            slots: vec![SlotConfig {
                selector: DeviceSelector::Index(0),
                layout_id: "XBOX_CONTROLLER".to_owned(),
                force_mouse: false,
            }],
            stone_database: None,
            gamecontroller_database: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SlotConfig {
    pub selector: DeviceSelector,
    pub layout_id: String,
    pub force_mouse: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeviceSelector {
    /// Index in SDL's current ordered gamepad enumeration.
    Index(usize),
    /// Exact SDL gamepad name.
    Name(String),
    /// Lowercase or uppercase SDL GUID string.
    Guid(String),
}

/// Owns the SDL thread. Dropping it neutralizes connected slots and shuts SDL down.
pub struct Sdl3Provider {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    states: Arc<Mutex<Vec<Option<PhysicalGamepadState>>>>,
    commands: mpsc::Sender<ProviderCommand>,
    pointer: Arc<Mutex<Option<[i16; 2]>>>,
}

#[derive(Clone, Debug)]
pub struct PhysicalGamepadState {
    pub name: String,
    pub guid: String,
    pub layout_id: String,
    pub buttons: Vec<(SdlButton, bool)>,
    pub axes: Vec<(SdlAxis, i16)>,
    pub mapped: GamepadState,
}

enum ProviderCommand {
    SetBinding {
        slot: usize,
        target: String,
        binding: InputBinding,
    },
    ClearBinding {
        slot: usize,
        target: String,
    },
    ResetBindings {
        slot: usize,
    },
}

impl Sdl3Provider {
    pub fn start(
        config: ProviderConfig,
        updates: Arc<LatestGamepadMailbox>,
    ) -> Result<Self, String> {
        validate_config(&config)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let states = Arc::new(Mutex::new(vec![None; config.slots.len()]));
        let thread_states = states.clone();
        let (commands, command_rx) = mpsc::channel();
        let pointer = Arc::new(Mutex::new(None));
        let thread_pointer = pointer.clone();
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("snowglass-sdl3-input".to_owned())
            .spawn(move || {
                run(
                    config,
                    updates,
                    thread_stop,
                    started_tx,
                    thread_states,
                    command_rx,
                    thread_pointer,
                )
            })
            .map_err(|error| format!("failed to start SDL3 input thread: {error}"))?;
        match started_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                stop,
                thread: Some(thread),
                states,
                commands,
                pointer,
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                let _ = thread.join();
                Err("SDL3 input thread exited during initialization".to_owned())
            }
        }
    }

    pub fn states(&self) -> Vec<Option<PhysicalGamepadState>> {
        self.states
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn set_binding(&self, slot: usize, target: String, binding: InputBinding) {
        let _ = self.commands.send(ProviderCommand::SetBinding {
            slot,
            target,
            binding,
        });
    }

    pub fn reset_bindings(&self, slot: usize) {
        let _ = self.commands.send(ProviderCommand::ResetBindings { slot });
    }

    pub fn clear_binding(&self, slot: usize, target: String) {
        let _ = self
            .commands
            .send(ProviderCommand::ClearBinding { slot, target });
    }

    /// Sets the absolute pointer carried by the first virtual gamepad. `None`
    /// neutralizes the axes. Dolphin mappings may bind or ignore these axes.
    pub fn set_pointer(&self, pointer: Option<[i16; 2]>) {
        *self
            .pointer
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = pointer;
    }
}

impl Drop for Sdl3Provider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn validate_config(config: &ProviderConfig) -> Result<(), String> {
    if config.slots.is_empty() || config.slots.len() > u8::MAX as usize + 1 {
        return Err("gamepad slot count must be 1..=256".to_owned());
    }
    let registry = load_stone_registry(config.stone_database.as_deref())?;
    for slot in &config.slots {
        if registry.layout(&slot.layout_id).is_none() {
            return Err(format!(
                "unknown Stone controller layout {}",
                slot.layout_id
            ));
        }
    }
    Ok(())
}

struct Pad {
    gamepad: Gamepad,
    identity: DeviceIdentity,
    layout_id: String,
    mapping: CompiledMapping,
    last: GamepadState,
    has_touchpad: bool,
    touchpad_pointer: Option<[i16; 2]>,
}

#[derive(Clone, Debug)]
struct DeviceIdentity {
    guid: String,
    vendor: Option<u16>,
    product: Option<u16>,
}

#[derive(Clone, Copy)]
struct AxisMap {
    source: SdlAxis,
    invert: bool,
}

#[derive(Clone)]
struct CompiledMapping {
    buttons: Vec<(SdlButton, GamepadButton)>,
    axes: [Option<AxisMap>; 6],
    digital_triggers: Vec<(SdlButton, usize)>,
    accelerometer: bool,
    gyroscope: bool,
}

fn run(
    config: ProviderConfig,
    updates: Arc<LatestGamepadMailbox>,
    stop: Arc<AtomicBool>,
    started: mpsc::SyncSender<Result<(), String>>,
    states: Arc<Mutex<Vec<Option<PhysicalGamepadState>>>>,
    commands: mpsc::Receiver<ProviderCommand>,
    pointer: Arc<Mutex<Option<[i16; 2]>>>,
) {
    let result = initialize(&config);
    let (sdl, subsystem, mut event_pump, registry, remappings) = match result {
        Ok(value) => value,
        Err(error) => {
            let _ = started.send(Err(error));
            return;
        }
    };
    let _sdl = sdl;
    let _ = started.send(Ok(()));
    let mut pads = (0..config.slots.len())
        .map(|_| None)
        .collect::<Vec<Option<Pad>>>();
    let mut sequences = vec![0u32; config.slots.len()];
    let mut last_published = vec![None::<GamepadState>; config.slots.len()];
    let mut next_topology_refresh = Instant::now();
    let mut interactive_remappings =
        vec![HashMap::<String, InputBinding>::new(); config.slots.len()];

    while !stop.load(Ordering::Acquire) {
        for event in event_pump.poll_iter() {
            update_touchpad_pointer(&mut pads, event);
        }
        subsystem.update();

        while let Ok(command) = commands.try_recv() {
            match command {
                ProviderCommand::SetBinding {
                    slot,
                    target,
                    binding,
                } => {
                    if let Some(bindings) = interactive_remappings.get_mut(slot) {
                        bindings.insert(target, binding);
                        recompile_pad(slot, &mut pads, &registry, &remappings, bindings);
                    }
                }
                ProviderCommand::ResetBindings { slot } => {
                    if let Some(bindings) = interactive_remappings.get_mut(slot) {
                        bindings.clear();
                        recompile_pad(slot, &mut pads, &registry, &remappings, bindings);
                    }
                }
                ProviderCommand::ClearBinding { slot, target } => {
                    if let Some(bindings) = interactive_remappings.get_mut(slot) {
                        bindings.remove(&target);
                        recompile_pad(slot, &mut pads, &registry, &remappings, bindings);
                    }
                }
            }
        }

        if Instant::now() >= next_topology_refresh {
            refresh_devices(
                &subsystem,
                &mut pads,
                &config.slots,
                &registry,
                &remappings,
                &interactive_remappings,
            );
            next_topology_refresh = Instant::now() + Duration::from_millis(100);
        }

        let mut snapshots = states.lock().unwrap_or_else(|error| error.into_inner());
        let pointer = pointer
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .unwrap_or_default();
        for (slot, pad) in pads.iter_mut().enumerate() {
            let mut state = if let Some(pad) = pad {
                let state = pad.mapping.read(&pad.gamepad);
                snapshots[slot] = Some(physical_state(pad, state));
                state
            } else {
                snapshots[slot] = None;
                GamepadState::default()
            };
            state.pointer = if config.slots[slot].force_mouse {
                pointer
            } else if let Some(pad) = pad {
                if pad.has_touchpad {
                    pad.touchpad_pointer.unwrap_or_default()
                } else {
                    pointer
                }
            } else {
                pointer
            };
            if last_published[slot] != Some(state) {
                sequences[slot] = sequences[slot].wrapping_add(1);
                last_published[slot] = Some(state);
                updates.publish(GamepadUpdate {
                    slot: GamepadSlot::new(slot as u8).unwrap(),
                    sequence: sequences[slot],
                    connected: true,
                    state,
                });
            }
        }
        drop(snapshots);
        // Do not impose a fixed polling interval on physical input. Yielding
        // lets another ready thread run while making this provider eligible to
        // resume immediately instead of waiting for a timer quantum.
        thread::yield_now();
    }

    for (slot, pad) in pads.iter_mut().enumerate() {
        let _ = pad.take();
        sequences[slot] = sequences[slot].wrapping_add(1);
        updates.publish(GamepadUpdate {
            slot: GamepadSlot::new(slot as u8).unwrap(),
            sequence: sequences[slot],
            connected: false,
            state: GamepadState::default(),
        });
        states.lock().unwrap_or_else(|error| error.into_inner())[slot] = None;
    }
}

type Initialized = (
    sdl3::Sdl,
    sdl3::GamepadSubsystem,
    sdl3::EventPump,
    StoneRegistry,
    RemapDatabase,
);

fn initialize(config: &ProviderConfig) -> Result<Initialized, String> {
    let registry = load_stone_registry(config.stone_database.as_deref())?;
    let remappings = RemapDatabase::default();
    let sdl = sdl3::init().map_err(|error| format!("failed to initialize SDL3: {error}"))?;
    let subsystem = sdl
        .gamepad()
        .map_err(|error| format!("failed to initialize SDL3 gamepads: {error}"))?;
    if let Some(path) = &config.gamecontroller_database {
        subsystem
            .load_mappings(path)
            .map_err(|error| format!("failed to load {}: {error}", path.display()))?;
    } else {
        subsystem
            .load_mappings_from_read(&mut EMBEDDED_GAMECONTROLLER_DB.as_bytes())
            .map_err(|error| format!("failed to load embedded gamecontrollerdb.txt: {error}"))?;
    }
    let event_pump = sdl
        .event_pump()
        .map_err(|error| format!("failed to create SDL3 event pump: {error}"))?;
    Ok((sdl, subsystem, event_pump, registry, remappings))
}

fn refresh_devices(
    subsystem: &sdl3::GamepadSubsystem,
    pads: &mut [Option<Pad>],
    slots: &[SlotConfig],
    registry: &StoneRegistry,
    remappings: &RemapDatabase,
    interactive_remappings: &[HashMap<String, InputBinding>],
) {
    for entry in pads.iter_mut() {
        let disconnected = entry.as_ref().is_some_and(|pad| !pad.gamepad.connected());
        if disconnected {
            let _pad = entry.take().unwrap();
        }
    }
    let Ok(ids) = subsystem.gamepads() else {
        return;
    };
    let mut used = pads
        .iter()
        .flatten()
        .filter_map(|pad| pad.gamepad.id().ok())
        .collect::<std::collections::HashSet<_>>();
    for slot in 0..pads.len() {
        if pads[slot].is_some() {
            continue;
        }
        let Some(id) = select_device(subsystem, &ids, &used, &slots[slot].selector) else {
            continue;
        };
        let identity = DeviceIdentity {
            guid: subsystem.guid_for_id(id).to_string().to_ascii_lowercase(),
            vendor: subsystem.vendor_for_id(id),
            product: subsystem.product_for_id(id),
        };
        match subsystem.open(id) {
            Ok(gamepad) => {
                let has_touchpad = gamepad.touchpads_count() != 0;
                let layout = registry
                    .layout(&slots[slot].layout_id)
                    .expect("validated slot layout");
                enable_layout_sensors(&gamepad, layout);
                let mapping = compiled_with_overrides(
                    &gamepad,
                    &identity,
                    layout,
                    remappings,
                    &interactive_remappings[slot],
                );
                let state = mapping.read(&gamepad);
                pads[slot] = Some(Pad {
                    gamepad,
                    identity,
                    layout_id: slots[slot].layout_id.clone(),
                    mapping,
                    last: state,
                    has_touchpad,
                    touchpad_pointer: None,
                });
                used.insert(id);
            }
            Err(error) => eprintln!("snowglass: failed to open SDL3 gamepad: {error}"),
        }
    }
}

fn update_touchpad_pointer(pads: &mut [Option<Pad>], event: Event) {
    let (which, touchpad, finger, x, y) = match event {
        Event::ControllerTouchpadDown {
            which,
            touchpad,
            finger,
            x,
            y,
            ..
        }
        | Event::ControllerTouchpadMotion {
            which,
            touchpad,
            finger,
            x,
            y,
            ..
        }
        | Event::ControllerTouchpadUp {
            which,
            touchpad,
            finger,
            x,
            y,
            ..
        } => (which, touchpad, finger, x, y),
        _ => return,
    };
    if touchpad != 0 || finger != 0 {
        return;
    }
    let Some(pad) = pads.iter_mut().flatten().find(|pad| {
        pad.gamepad
            .id()
            .is_ok_and(|instance_id| instance_id == which)
    }) else {
        return;
    };
    pad.touchpad_pointer = normalized_pointer_axes(f64::from(x), f64::from(y));
}

fn normalized_pointer_axes(x: f64, y: f64) -> Option<[i16; 2]> {
    fn axis(value: f64) -> Option<i16> {
        if !value.is_finite() {
            return None;
        }
        let value = value.clamp(0.0, 1.0);
        let scaled = if value <= 0.5 {
            value * 65_536.0 - 32_768.0
        } else {
            (value - 0.5) * 65_534.0
        }
        .round();
        Some(scaled.clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16)
    }
    Some([axis(x)?, axis(y)?])
}

fn select_device(
    subsystem: &sdl3::GamepadSubsystem,
    ids: &[sdl3::joystick::JoystickId],
    used: &std::collections::HashSet<sdl3::joystick::JoystickId>,
    selector: &DeviceSelector,
) -> Option<sdl3::joystick::JoystickId> {
    match selector {
        DeviceSelector::Index(index) => ids.get(*index).copied().filter(|id| !used.contains(id)),
        DeviceSelector::Name(name) => ids.iter().copied().find(|id| {
            !used.contains(id) && subsystem.name_for_id(*id).ok().as_deref() == Some(name)
        }),
        DeviceSelector::Guid(guid) => ids.iter().copied().find(|id| {
            !used.contains(id)
                && subsystem
                    .guid_for_id(*id)
                    .to_string()
                    .eq_ignore_ascii_case(guid)
        }),
    }
}

fn recompile_pad(
    slot: usize,
    pads: &mut [Option<Pad>],
    registry: &StoneRegistry,
    remappings: &RemapDatabase,
    overrides: &HashMap<String, InputBinding>,
) {
    let Some(pad) = pads.get_mut(slot).and_then(Option::as_mut) else {
        return;
    };
    let layout = registry
        .layout(&pad.layout_id)
        .expect("pinned layout remains available");
    pad.mapping =
        compiled_with_overrides(&pad.gamepad, &pad.identity, layout, remappings, overrides);
    pad.last.buttons = !pad.last.buttons;
}

fn compiled_with_overrides(
    gamepad: &Gamepad,
    identity: &DeviceIdentity,
    layout: &StoneLayout,
    remappings: &RemapDatabase,
    overrides: &HashMap<String, InputBinding>,
) -> CompiledMapping {
    let mut effective = remappings.clone();
    effective.bindings.extend(overrides.clone());
    CompiledMapping::new(gamepad, identity, layout, &effective)
}

fn physical_state(pad: &Pad, mapped: GamepadState) -> PhysicalGamepadState {
    PhysicalGamepadState {
        name: pad
            .gamepad
            .name()
            .unwrap_or_else(|| "Unknown gamepad".to_owned()),
        guid: pad.identity.guid.clone(),
        layout_id: pad.layout_id.clone(),
        buttons: ALL_BUTTONS
            .into_iter()
            .map(|button| (button, pad.gamepad.button(to_sdl_button(button))))
            .collect(),
        axes: ALL_AXES
            .into_iter()
            .map(|axis| (axis, pad.gamepad.axis(to_sdl_axis(axis))))
            .collect(),
        mapped,
    }
}

impl CompiledMapping {
    fn new(
        gamepad: &Gamepad,
        identity: &DeviceIdentity,
        layout: &StoneLayout,
        database: &RemapDatabase,
    ) -> Self {
        let mut targets = HashMap::<String, InputBinding>::new();
        for button in ALL_BUTTONS {
            if let Some(target) = default_button_target(gamepad, button, layout) {
                targets.insert(target.to_owned(), InputBinding::Button { button });
            }
        }
        apply_default_axes(&mut targets, layout);
        targets.extend(database.bindings.clone());
        for profile in &database.profiles {
            if profile_matches(profile, identity, layout) {
                targets.extend(profile.bindings.clone());
            }
        }

        let mut buttons = Vec::new();
        let mut axes = DEFAULT_AXES;
        let mut digital_triggers = Vec::new();
        for (element, binding) in targets {
            if !layout.has(&element) {
                continue;
            }
            if let Some(target) = stone_button(&element) {
                if let InputBinding::Button { button } = binding {
                    buttons.retain(|(_, existing)| *existing != target);
                    buttons.push((button, target));
                }
                continue;
            }
            let Some(index) = stone_axis_index(&element) else {
                continue;
            };
            match binding {
                InputBinding::Axis { axis, invert } => {
                    axes[index] = Some(AxisMap {
                        source: axis,
                        invert,
                    })
                }
                InputBinding::Button { button } if index >= 4 => {
                    axes[index] = None;
                    digital_triggers.retain(|(_, existing)| *existing != index);
                    digital_triggers.push((button, index));
                }
                InputBinding::Disabled { disabled: true } => {
                    axes[index] = None;
                }
                _ => {}
            }
        }
        Self {
            buttons,
            axes,
            digital_triggers,
            accelerometer: layout.has("AccelerometerAxisPositiveX")
                || layout.has("AccelerometerAxisNegativeX"),
            gyroscope: layout.has("GyroscopeAxisPositiveX") || layout.has("GyroscopeAxisNegativeX"),
        }
    }

    fn read(&self, gamepad: &Gamepad) -> GamepadState {
        let mut state = GamepadState::default();
        for &(source, target) in &self.buttons {
            if gamepad.button(to_sdl_button(source)) {
                state.buttons |= target.mask();
            }
        }
        let axis = |mapping: Option<AxisMap>| {
            mapping.map_or(0, |mapping| {
                let value = gamepad.axis(to_sdl_axis(mapping.source));
                if mapping.invert {
                    value.saturating_neg()
                } else {
                    value
                }
            })
        };
        state.left_stick = [axis(self.axes[0]), axis(self.axes[1])];
        state.right_stick = [axis(self.axes[2]), axis(self.axes[3])];
        state.left_trigger = trigger(axis(self.axes[4]));
        state.right_trigger = trigger(axis(self.axes[5]));
        for &(button, target) in &self.digital_triggers {
            if gamepad.button(to_sdl_button(button)) {
                if target == 4 {
                    state.left_trigger = u16::MAX;
                } else {
                    state.right_trigger = u16::MAX;
                }
            }
        }
        if self.accelerometer {
            let _ = gamepad.sensor_get_data(SensorType::Accelerometer, &mut state.accelerometer);
        }
        if self.gyroscope {
            let _ = gamepad.sensor_get_data(SensorType::Gyroscope, &mut state.gyroscope);
        }
        state
    }
}

fn enable_layout_sensors(gamepad: &Gamepad, layout: &StoneLayout) {
    for (enabled, sensor) in [
        (
            layout.has("AccelerometerAxisPositiveX") || layout.has("AccelerometerAxisNegativeX"),
            SensorType::Accelerometer,
        ),
        (
            layout.has("GyroscopeAxisPositiveX") || layout.has("GyroscopeAxisNegativeX"),
            SensorType::Gyroscope,
        ),
    ] {
        if enabled {
            let _ = gamepad.sensor_set_enabled(sensor, true);
        }
    }
}

const DEFAULT_AXES: [Option<AxisMap>; 6] = [
    Some(AxisMap {
        source: SdlAxis::LeftX,
        invert: false,
    }),
    Some(AxisMap {
        source: SdlAxis::LeftY,
        invert: false,
    }),
    Some(AxisMap {
        source: SdlAxis::RightX,
        invert: false,
    }),
    Some(AxisMap {
        source: SdlAxis::RightY,
        invert: false,
    }),
    Some(AxisMap {
        source: SdlAxis::LeftTrigger,
        invert: false,
    }),
    Some(AxisMap {
        source: SdlAxis::RightTrigger,
        invert: false,
    }),
];

const ALL_BUTTONS: [SdlButton; 21] = [
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

const ALL_AXES: [SdlAxis; 6] = [
    SdlAxis::LeftX,
    SdlAxis::LeftY,
    SdlAxis::RightX,
    SdlAxis::RightY,
    SdlAxis::LeftTrigger,
    SdlAxis::RightTrigger,
];

fn default_button_target<'a>(
    gamepad: &Gamepad,
    source: SdlButton,
    layout: &'a StoneLayout,
) -> Option<&'a str> {
    let fixed = match source {
        SdlButton::Back => Some("ButtonSelect"),
        SdlButton::Guide => Some("ButtonGuide"),
        SdlButton::Start => Some("ButtonStart"),
        SdlButton::LeftStick => Some("ButtonClickL"),
        SdlButton::RightStick => Some("ButtonClickR"),
        SdlButton::LeftShoulder => Some("ButtonL"),
        SdlButton::RightShoulder => Some("ButtonR"),
        SdlButton::DpadUp => Some("DirectionalN"),
        SdlButton::DpadDown => Some("DirectionalS"),
        SdlButton::DpadLeft => Some("DirectionalW"),
        SdlButton::DpadRight => Some("DirectionalE"),
        SdlButton::Misc1 => Some("Button0"),
        SdlButton::Touchpad => Some("Button0"),
        _ => None,
    };
    if let Some(target) = fixed.filter(|target| layout.has(target)) {
        return Some(target);
    }
    let label = match gamepad.button_label_for_gamepad_type(to_sdl_button(source)) {
        ButtonLabel::A => "A",
        ButtonLabel::B => "B",
        ButtonLabel::X => "X",
        ButtonLabel::Y => "Y",
        ButtonLabel::Cross => "Cross",
        ButtonLabel::Circle => "Circle",
        ButtonLabel::Square => "Square",
        ButtonLabel::Triangle => "Triangle",
        ButtonLabel::Unknown => return positional_face_target(source, layout),
    };
    layout
        .elements
        .iter()
        .find(|element| element.label.eq_ignore_ascii_case(label))
        .map(|element| element.id.as_str())
        .or_else(|| positional_face_target(source, layout))
}

fn positional_face_target(source: SdlButton, layout: &StoneLayout) -> Option<&str> {
    let target = match source {
        SdlButton::South => "ButtonA",
        SdlButton::East => "ButtonB",
        SdlButton::West => "ButtonX",
        SdlButton::North => "ButtonY",
        _ => return None,
    };
    layout.has(target).then_some(target)
}

fn apply_default_axes(targets: &mut HashMap<String, InputBinding>, layout: &StoneLayout) {
    for (element, axis) in [
        ("AxisLeftAnalogPositiveX", SdlAxis::LeftX),
        ("AxisLeftAnalogNegativeX", SdlAxis::LeftX),
        ("AxisLeftAnalogPositiveY", SdlAxis::LeftY),
        ("AxisLeftAnalogNegativeY", SdlAxis::LeftY),
        ("AxisRightAnalogPositiveX", SdlAxis::RightX),
        ("AxisRightAnalogNegativeX", SdlAxis::RightX),
        ("AxisRightAnalogPositiveY", SdlAxis::RightY),
        ("AxisRightAnalogNegativeY", SdlAxis::RightY),
        ("TriggerLeft", SdlAxis::LeftTrigger),
        ("TriggerRight", SdlAxis::RightTrigger),
    ] {
        if layout.has(element) {
            targets.insert(
                element.to_owned(),
                InputBinding::Axis {
                    axis,
                    invert: false,
                },
            );
        }
    }
}

fn profile_matches(
    profile: &RemapProfile,
    identity: &DeviceIdentity,
    layout: &StoneLayout,
) -> bool {
    profile
        .guid
        .as_ref()
        .is_none_or(|guid| guid.eq_ignore_ascii_case(&identity.guid))
        && profile
            .vendor
            .is_none_or(|vendor| Some(vendor) == identity.vendor)
        && profile
            .product
            .is_none_or(|product| Some(product) == identity.product)
        && profile.layout.as_ref().is_none_or(|id| id == &layout.id)
}

fn stone_button(element: &str) -> Option<GamepadButton> {
    Some(match element {
        "ButtonA" => GamepadButton::A,
        "ButtonB" => GamepadButton::B,
        "ButtonX" => GamepadButton::X,
        "ButtonY" => GamepadButton::Y,
        "ButtonSelect" => GamepadButton::Select,
        "ButtonGuide" => GamepadButton::Guide,
        "ButtonStart" => GamepadButton::Start,
        "ButtonClickL" => GamepadButton::LeftStick,
        "ButtonClickR" => GamepadButton::RightStick,
        "ButtonL" => GamepadButton::L,
        "ButtonR" => GamepadButton::R,
        "DirectionalN" => GamepadButton::DpadUp,
        "DirectionalS" => GamepadButton::DpadDown,
        "DirectionalW" => GamepadButton::DpadLeft,
        "DirectionalE" => GamepadButton::DpadRight,
        "Button0" => GamepadButton::Misc1,
        _ => return None,
    })
}

fn stone_axis_index(element: &str) -> Option<usize> {
    match element {
        "AxisLeftAnalogPositiveX" | "AxisLeftAnalogNegativeX" => Some(0),
        "AxisLeftAnalogPositiveY" | "AxisLeftAnalogNegativeY" => Some(1),
        "AxisRightAnalogPositiveX" | "AxisRightAnalogNegativeX" => Some(2),
        "AxisRightAnalogPositiveY" | "AxisRightAnalogNegativeY" => Some(3),
        "TriggerLeft" => Some(4),
        "TriggerRight" => Some(5),
        _ => None,
    }
}

fn trigger(value: i16) -> u16 {
    let value = u32::from(value.max(0) as u16);
    ((value * u32::from(u16::MAX)) / i16::MAX as u32) as u16
}

fn to_sdl_button(button: SdlButton) -> Button {
    match button {
        SdlButton::South => Button::South,
        SdlButton::East => Button::East,
        SdlButton::West => Button::West,
        SdlButton::North => Button::North,
        SdlButton::Back => Button::Back,
        SdlButton::Guide => Button::Guide,
        SdlButton::Start => Button::Start,
        SdlButton::LeftStick => Button::LeftStick,
        SdlButton::RightStick => Button::RightStick,
        SdlButton::LeftShoulder => Button::LeftShoulder,
        SdlButton::RightShoulder => Button::RightShoulder,
        SdlButton::DpadUp => Button::DPadUp,
        SdlButton::DpadDown => Button::DPadDown,
        SdlButton::DpadLeft => Button::DPadLeft,
        SdlButton::DpadRight => Button::DPadRight,
        SdlButton::Misc1 => Button::Misc1,
        SdlButton::RightPaddle1 => Button::RightPaddle1,
        SdlButton::LeftPaddle1 => Button::LeftPaddle1,
        SdlButton::RightPaddle2 => Button::RightPaddle2,
        SdlButton::LeftPaddle2 => Button::LeftPaddle2,
        SdlButton::Touchpad => Button::Touchpad,
    }
}

fn to_sdl_axis(axis: SdlAxis) -> Axis {
    match axis {
        SdlAxis::LeftX => Axis::LeftX,
        SdlAxis::LeftY => Axis::LeftY,
        SdlAxis::RightX => Axis::RightX,
        SdlAxis::RightY => Axis::RightY,
        SdlAxis::LeftTrigger => Axis::TriggerLeft,
        SdlAxis::RightTrigger => Axis::TriggerRight,
    }
}

fn load_stone_registry(path: Option<&Path>) -> Result<StoneRegistry, String> {
    let json = match path {
        Some(path) => fs::read_to_string(path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?,
        None => EMBEDDED_STONE_JSON.to_owned(),
    };
    StoneRegistry::from_json(&json)
}

#[cfg(test)]
fn validate_remappings(database: &RemapDatabase, layouts: &[&StoneLayout]) -> Result<(), String> {
    for target in database.bindings.keys() {
        if !layouts.iter().any(|layout| layout.has(target)) {
            return Err(format!(
                "remapping target {target} is absent from every configured layout"
            ));
        }
    }
    if database
        .bindings
        .values()
        .any(|binding| matches!(binding, InputBinding::Disabled { disabled: false }))
    {
        return Err("disabled bindings must set disabled to true".to_owned());
    }
    for profile in &database.profiles {
        let matching = layouts
            .iter()
            .copied()
            .filter(|layout| profile.layout.as_deref().is_none_or(|id| id == layout.id))
            .collect::<Vec<_>>();
        if matching.is_empty() {
            continue;
        }
        for target in profile.bindings.keys() {
            if !matching.iter().any(|layout| layout.has(target)) {
                return Err(format!(
                    "remapping target {target} is absent from matching configured layouts"
                ));
            }
        }
        if profile
            .bindings
            .values()
            .any(|binding| matches!(binding, InputBinding::Disabled { disabled: false }))
        {
            return Err("disabled bindings must set disabled to true".to_owned());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_face_labels_are_not_positional() {
        let registry = StoneRegistry::from_json(EMBEDDED_STONE_JSON).unwrap();
        let layout = registry.layout("SWITCH_PRO_CONTROLLER").unwrap();
        assert_eq!(
            layout.elements.iter().find(|e| e.label == "B").unwrap().id,
            "ButtonB"
        );
        assert_eq!(stone_button("ButtonB"), Some(GamepadButton::B));
    }

    #[test]
    fn trigger_conversion_covers_the_wire_range() {
        assert_eq!(trigger(i16::MIN), 0);
        assert_eq!(trigger(0), 0);
        assert_eq!(trigger(i16::MAX), u16::MAX);
    }

    #[test]
    fn touchpad_coordinates_fill_pointer_axes() {
        assert_eq!(normalized_pointer_axes(0.0, 0.0), Some([i16::MIN, i16::MIN]));
        assert_eq!(normalized_pointer_axes(0.5, 0.5), Some([0, 0]));
        assert_eq!(normalized_pointer_axes(1.0, 1.0), Some([i16::MAX, i16::MAX]));
    }

    #[test]
    fn rejects_user_targets_outside_the_selected_layout() {
        let registry = StoneRegistry::from_json(EMBEDDED_STONE_JSON).unwrap();
        let remap = RemapDatabase::from_json(
            r#"{"bindings":{"DefinitelyNotAStoneElement":{"button":"south"}}}"#,
        )
        .unwrap();
        assert!(
            validate_remappings(&remap, &[registry.layout("XBOX_CONTROLLER").unwrap()]).is_err()
        );
    }

    #[test]
    fn provider_initializes_without_a_connected_gamepad() {
        let mailbox = Arc::new(LatestGamepadMailbox::new(1));
        let provider = Sdl3Provider::start(ProviderConfig::default(), mailbox).unwrap();
        drop(provider);
    }
}
