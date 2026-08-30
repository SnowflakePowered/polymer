//! SDL3 physical-gamepad capture normalized into element-labelled snapshots.

#[cfg(test)]
use crate::gamepad::LatestGamepadMailbox;
use crate::{
    gamepad::{GamepadButton, GamepadSlot, GamepadState, GamepadUpdate, GamepadUpdateSink},
    hub::{AxisTransform, PointerAxis, SourceControl, SourceSelector},
    layout::{
        EMBEDDED_DEFAULT_KEYBOARD_MAPPING, EMBEDDED_GAMECONTROLLER_DB, InputBinding, InputLayout,
        KeyboardMapping, ProjectionMode, RemapDatabase, RemapProfile, SdlAxis, SdlButton,
    },
    sdl::Wire,
};
use consolespec::input::{
    Alignment, AnalogClass, Axis, BindingKey, ButtonElement, Component, Sign,
};
use parking_lot::Mutex;
use sdl3::{
    EventSubsystem,
    event::{Event, EventType},
};
// SDL's own axis enum names the six gamepad axes; consolespec's `Axis` names a
// degree of freedom of a cluster. Both appear here, so SDL's is qualified.
use sdl3::gamepad::{Axis as SdlNativeAxis, Button, ButtonLabel, Gamepad};
use sdl3::sensor::SensorType;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

mod source_registry;
use source_registry::{DeviceFingerprint, DeviceIdentity, SourceRegistry};

/// Keep keyboard, mouse, and physical-pad latency below a display frame without
/// monopolizing a CPU core when no SDL events are waiting.
const INPUT_POLL_INTERVAL: Duration = Duration::from_millis(1);

/// SDL can enqueue a large initial burst while a newly opened HID device
/// publishes its first axis and sensor samples. Those values are polled from
/// the gamepad handle below, so event delivery is redundant. Keep a defensive
/// bound for the touchpad and topology events that remain enabled so commands
/// and state publication cannot be starved by an event-producing device.
const MAX_EVENTS_PER_POLL: usize = 128;

#[derive(Clone, Debug, Default)]
pub struct ProviderConfig {
    pub slots: Vec<SlotConfig>,
    pub gamecontroller_database: Option<PathBuf>,
    pub keyboard_mapping_directory: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SlotConfig {
    /// The durable physical source assigned to this virtual controller.
    /// `None` leaves the virtual controller disconnected.
    pub source: Option<SourceSelector>,
    pub layout_id: String,
    pub force_mouse: bool,
    pub projection_mode: ProjectionMode,
}

/// Owns the SDL thread. Dropping it neutralizes connected slots and shuts SDL down.
pub struct Sdl3Provider {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    states: Arc<Mutex<Vec<Option<PhysicalGamepadState>>>>,
    devices: Arc<Mutex<Vec<PhysicalDeviceState>>>,
    commands: mpsc::Sender<ProviderCommand>,
    pointer: Arc<Mutex<PointerSample>>,
    mapped_keys: Arc<Mutex<HashSet<String>>>,
}

#[derive(Clone, Debug)]
pub struct PhysicalGamepadState {
    /// Provider-owned identity retained when this physical device reconnects.
    pub device_id: String,
    pub name: String,
    pub guid: String,
    pub vendor: Option<u16>,
    pub product: Option<u16>,
    pub layout_id: String,
    pub buttons: Vec<(SdlButton, bool)>,
    pub axes: Vec<(SdlAxis, i16)>,
    /// Physical controls consumed by the active layout mapping.
    pub mapped_buttons: Vec<SdlButton>,
    pub mapped_axes: Vec<SdlAxis>,
    pub mapped: GamepadState,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PhysicalDeviceState {
    pub device_id: String,
    pub name: String,
    pub guid: String,
    pub vendor: Option<u16>,
    pub product: Option<u16>,
    pub serial: Option<String>,
    pub buttons: Vec<(SdlButton, bool)>,
    pub axes: Vec<(SdlAxis, i16)>,
}

enum ProviderCommand {
    SetEnabled(bool),
    ReconfigureSlots {
        slots: Vec<SlotConfig>,
        response: mpsc::SyncSender<Result<(), String>>,
    },
    SetBinding {
        slot: usize,
        target: BindingKey,
        binding: InputBinding,
    },
    SetKeyBinding {
        slot: usize,
        target: BindingKey,
        key: Option<String>,
    },
    SetMouseAxisBinding {
        slot: usize,
        target: BindingKey,
        source: SourceControl,
        transform: AxisTransform,
    },
    ClearBinding {
        slot: usize,
        target: BindingKey,
    },
    ResetBindings {
        slot: usize,
    },
    SetKey {
        key: String,
        pressed: bool,
    },
    ClearKeys,
}

impl Sdl3Provider {
    /// Enables or neutralizes mapped output without closing SDL devices.
    ///
    /// Physical diagnostic snapshots remain available while disabled.
    pub fn set_enabled(&self, enabled: bool) {
        let _ = self.commands.send(ProviderCommand::SetEnabled(enabled));
    }

    /// Replaces virtual-port routing without restarting SDL enumeration.
    ///
    /// Only slots whose routing actually changed are disconnected before their
    /// replacement is published. Unchanged SDL handles remain live.
    pub fn reconfigure_slots(&self, slots: Vec<SlotConfig>) -> Result<(), String> {
        if slots.len() > u8::MAX as usize + 1 {
            return Err("gamepad slot count must be 0..=256".to_owned());
        }
        let (response, result) = mpsc::sync_channel(1);
        self.commands
            .send(ProviderCommand::ReconfigureSlots { slots, response })
            .map_err(|_| "SDL3 input provider is no longer running".to_owned())?;
        result
            .recv()
            .map_err(|_| "SDL3 input provider exited during reconfiguration".to_owned())?
    }

    pub fn start(
        config: ProviderConfig,
        updates: Arc<dyn GamepadUpdateSink>,
    ) -> Result<Self, String> {
        Self::start_with_enabled(config, updates, true)
    }

    /// Starts physical discovery immediately, but keeps virtual output neutral
    /// until [`Self::set_enabled`] is called.
    pub fn start_disabled(
        config: ProviderConfig,
        updates: Arc<dyn GamepadUpdateSink>,
    ) -> Result<Self, String> {
        Self::start_with_enabled(config, updates, false)
    }

    fn start_with_enabled(
        config: ProviderConfig,
        updates: Arc<dyn GamepadUpdateSink>,
        initially_enabled: bool,
    ) -> Result<Self, String> {
        validate_config(&config)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let states = Arc::new(Mutex::new(vec![None; config.slots.len()]));
        let thread_states = states.clone();
        let devices = Arc::new(Mutex::new(Vec::new()));
        let thread_devices = devices.clone();
        let (commands, command_rx) = mpsc::channel();
        let pointer = Arc::new(Mutex::new(PointerSample::default()));
        let thread_pointer = pointer.clone();
        let mapped_keys = Arc::new(Mutex::new(HashSet::new()));
        let thread_mapped_keys = mapped_keys.clone();
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("snowglass-sdl3-input".to_owned())
            .spawn(move || {
                run(SdlWorkerArguments {
                    config,
                    updates,
                    stop: thread_stop,
                    started_tx,
                    states: thread_states,
                    devices: thread_devices,
                    commands: command_rx,
                    pointer: thread_pointer,
                    mapped_keys: thread_mapped_keys,
                    initially_enabled,
                })
            })
            .map_err(|error| format!("failed to start SDL3 input thread: {error}"))?;
        match started_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                stop,
                thread: Some(thread),
                states,
                devices,
                commands,
                pointer,
                mapped_keys,
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
        self.states.lock().clone()
    }

    pub fn devices(&self) -> Vec<PhysicalDeviceState> {
        self.devices.lock().clone()
    }

    pub fn set_binding(&self, slot: usize, target: BindingKey, binding: InputBinding) {
        let _ = self.commands.send(ProviderCommand::SetBinding {
            slot,
            target,
            binding,
        });
    }

    pub fn set_key_binding(&self, slot: usize, target: BindingKey, key: Option<String>) {
        let _ = self
            .commands
            .send(ProviderCommand::SetKeyBinding { slot, target, key });
    }

    pub fn set_mouse_axis_binding(
        &self,
        slot: usize,
        target: BindingKey,
        source: SourceControl,
        transform: AxisTransform,
    ) {
        let _ = self.commands.send(ProviderCommand::SetMouseAxisBinding {
            slot,
            target,
            source,
            transform,
        });
    }

    pub fn reset_bindings(&self, slot: usize) {
        let _ = self.commands.send(ProviderCommand::ResetBindings { slot });
    }

    pub fn clear_binding(&self, slot: usize, target: BindingKey) {
        let _ = self
            .commands
            .send(ProviderCommand::ClearBinding { slot, target });
    }

    pub fn set_key(&self, key: String, pressed: bool) {
        let _ = self.commands.send(ProviderCommand::SetKey { key, pressed });
    }

    pub fn clear_keys(&self) {
        let _ = self.commands.send(ProviderCommand::ClearKeys);
    }

    pub fn is_key_mapped(&self, key: &str) -> bool {
        self.mapped_keys.lock().contains(key)
    }

    /// Sets the absolute pointer carried by the first virtual gamepad. `None`
    /// neutralizes the axes. Dolphin mappings may bind or ignore these axes.
    pub fn set_pointer(&self, pointer: Option<[i16; 2]>) {
        self.pointer.lock().set_position(pointer);
    }

    /// Adds raw relative mouse motion. One hundred device counts maps to a
    /// full-scale axis before the binding's scale/dead-zone transform.
    pub fn add_mouse_delta(&self, delta: [f64; 2]) {
        self.pointer.lock().set_relative(delta, Instant::now());
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
    if config.slots.len() > u8::MAX as usize + 1 {
        return Err("gamepad slot count must be 0..=256".to_owned());
    }
    for slot in &config.slots {
        if InputLayout::get(&slot.layout_id).is_none() {
            return Err(format!("unknown input layout {}", slot.layout_id));
        }
    }
    Ok(())
}

struct Pad {
    gamepad: Rc<Gamepad>,
    identity: DeviceIdentity,
    layout_id: String,
    projection_mode: ProjectionMode,
    mapping: CompiledMapping,
    last: GamepadState,
    has_touchpad: bool,
    touchpad_pointer: Option<[i16; 2]>,
}

struct ObservedPad {
    gamepad: Rc<Gamepad>,
    identity: DeviceIdentity,
    name: String,
}

#[derive(Clone, Copy, Debug)]
struct PointerSample {
    position: Option<[i16; 2]>,
    delta: [f32; 2],
    moved_at: Instant,
}

impl Default for PointerSample {
    fn default() -> Self {
        Self {
            position: None,
            delta: [0.0; 2],
            moved_at: Instant::now(),
        }
    }
}

impl PointerSample {
    fn set_position(&mut self, position: Option<[i16; 2]>) {
        self.position = position;
    }

    fn set_relative(&mut self, delta: [f64; 2], now: Instant) {
        const COUNTS_FOR_FULL_AXIS: f64 = 100.0;
        self.delta = [
            (delta[0] / COUNTS_FOR_FULL_AXIS).clamp(-1.0, 1.0) as f32,
            (delta[1] / COUNTS_FOR_FULL_AXIS).clamp(-1.0, 1.0) as f32,
        ];
        self.moved_at = now;
    }

    fn relative(self, axis: PointerAxis, now: Instant) -> f32 {
        let elapsed = now.saturating_duration_since(self.moved_at);
        let weight = relative_mouse_weight(elapsed);
        self.delta[pointer_axis_index(axis)] * weight
    }

    fn absolute(self, axis: PointerAxis) -> f32 {
        self.position
            .map(|position| f32::from(position[pointer_axis_index(axis)]) / 32_767.0)
            .unwrap_or_default()
            .clamp(-1.0, 1.0)
    }
}

const RELATIVE_MOUSE_HOLD: Duration = Duration::from_millis(16);
const RELATIVE_MOUSE_RELEASE: Duration = Duration::from_millis(50);

fn relative_mouse_weight(elapsed: Duration) -> f32 {
    if elapsed <= RELATIVE_MOUSE_HOLD {
        return 1.0;
    }
    if elapsed >= RELATIVE_MOUSE_RELEASE {
        return 0.0;
    }
    let elapsed_ms = elapsed.as_millis() as f32;
    let hold_ms = RELATIVE_MOUSE_HOLD.as_millis() as f32;
    let release_ms = RELATIVE_MOUSE_RELEASE.as_millis() as f32;
    1.0 - (elapsed_ms - hold_ms) / (release_ms - hold_ms)
}

const fn pointer_axis_index(axis: PointerAxis) -> usize {
    match axis {
        PointerAxis::X => 0,
        PointerAxis::Y => 1,
    }
}

#[derive(Clone)]
struct MouseAxisMap {
    source: SourceControl,
    transform: AxisTransform,
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

#[derive(Clone)]
struct CompiledKeyboardMapping {
    /// Which device the bindings below name. Layouts are compile-time data, so
    /// holding one costs nothing and spares every caller from passing it.
    layout: &'static InputLayout,
    bindings: Vec<(BindingKey, String, KeyboardTarget)>,
}

#[derive(Clone, Copy)]
enum KeyboardTarget {
    Button(GamepadButton),
    Axis { index: usize, direction: i8 },
    Trigger { index: usize },
}

impl CompiledKeyboardMapping {
    fn new(mapping: KeyboardMapping, layout: &'static InputLayout) -> Result<Self, String> {
        let mut bindings = Vec::new();
        for (element, key) in mapping.bindings {
            // The document writes canonical keys, so one default mapping serves
            // every device; what it binds to is this device's own key.
            let Some(element) = layout.resolve(element) else {
                continue;
            };
            if key.trim().is_empty() {
                return Err(format!("keyboard binding {element} has an empty key"));
            }
            let Some(target) = keyboard_target(layout, element) else {
                continue;
            };
            bindings.push((element, key, target));
        }
        Ok(Self { layout, bindings })
    }

    fn set(&mut self, element: BindingKey, key: Option<String>) {
        self.bindings
            .retain(|(existing, _, _)| *existing != element);
        if let (Some(key), Some(target)) = (key, keyboard_target(self.layout, element)) {
            self.bindings.push((element, key, target));
        }
    }

    fn restore(&mut self, element: BindingKey, defaults: Option<&Self>) {
        let binding = defaults.and_then(|defaults| {
            defaults
                .bindings
                .iter()
                .find(|(existing, _, _)| *existing == element)
                .cloned()
        });
        self.bindings
            .retain(|(existing, _, _)| *existing != element);
        if let Some(binding) = binding {
            self.bindings.push(binding);
        }
    }

    fn state(&self, pressed: &HashSet<String>) -> GamepadState {
        let mut state = GamepadState::default();
        let mut axes = [0i32; 4];
        for (_, key, target) in &self.bindings {
            if !pressed.contains(key) {
                continue;
            }
            match *target {
                KeyboardTarget::Button(button) => state.buttons |= button.mask(),
                KeyboardTarget::Axis { index, direction } => {
                    axes[index] += i32::from(direction);
                }
                KeyboardTarget::Trigger { index: 4 } => state.left_trigger = u16::MAX,
                KeyboardTarget::Trigger { index: 5 } => state.right_trigger = u16::MAX,
                KeyboardTarget::Trigger { .. } => {}
            }
        }
        let axis = |index: usize| match axes[index].signum() {
            -1 => i16::MIN,
            1 => i16::MAX,
            _ => 0,
        };
        state.left_stick = [axis(0), axis(1)];
        state.right_stick = [axis(2), axis(3)];
        state
    }
}

fn keyboard_target(layout: &InputLayout, element: BindingKey) -> Option<KeyboardTarget> {
    if let Some(button) = element_button(layout, element) {
        return Some(KeyboardTarget::Button(button));
    }
    let index = element_axis_index(layout, element)?;
    if index >= 4 {
        return Some(KeyboardTarget::Trigger { index });
    }
    let positive = matches!(
        element,
        BindingKey::Analog {
            component: Component {
                sign: Sign::Positive,
                ..
            },
            ..
        }
    );
    let direction = match (index % 2, positive) {
        (1, true) => -1,
        (1, false) => 1,
        (_, true) => 1,
        (_, false) => -1,
    };
    Some(KeyboardTarget::Axis { index, direction })
}

fn apply_mouse_axes(
    state: &mut GamepadState,
    layout: &InputLayout,
    mappings: &HashMap<BindingKey, MouseAxisMap>,
    pointer: PointerSample,
    now: Instant,
) {
    let mut values = [0.0f32; 6];
    let mut assigned = [false; 6];
    for (&target, mapping) in mappings {
        let Some(index) = element_axis_index(layout, target) else {
            continue;
        };
        let value = match &mapping.source {
            SourceControl::MouseDelta { axis } => pointer.relative(*axis, now),
            SourceControl::MousePosition { axis } => pointer.absolute(*axis),
            _ => continue,
        };
        let value = transform_axis(value, mapping.transform);
        if !assigned[index] || value.abs() > values[index].abs() {
            values[index] = value;
            assigned[index] = true;
        }
    }
    let axis = |index| normalized_axis(values[index]);
    if assigned[0] {
        state.left_stick[0] = axis(0);
    }
    if assigned[1] {
        state.left_stick[1] = axis(1);
    }
    if assigned[2] {
        state.right_stick[0] = axis(2);
    }
    if assigned[3] {
        state.right_stick[1] = axis(3);
    }
    if assigned[4] {
        state.left_trigger = trigger(axis(4));
    }
    if assigned[5] {
        state.right_trigger = trigger(axis(5));
    }
}

fn transform_axis(value: f32, transform: AxisTransform) -> f32 {
    let mut value = value.clamp(-1.0, 1.0);
    let magnitude = value.abs();
    value = if magnitude <= transform.dead_zone {
        0.0
    } else {
        value.signum() * (magnitude - transform.dead_zone) / (1.0 - transform.dead_zone)
    };
    if transform.invert {
        value = -value;
    }
    (value * transform.scale).clamp(-1.0, 1.0)
}

fn normalized_axis(value: f32) -> i16 {
    let value = value.clamp(-1.0, 1.0);
    if value <= 0.0 {
        (value * 32_768.0).round() as i16
    } else {
        (value * 32_767.0).round() as i16
    }
}

struct SdlWorkerArguments {
    config: ProviderConfig,
    updates: Arc<dyn GamepadUpdateSink>,
    stop: Arc<AtomicBool>,
    started_tx: mpsc::SyncSender<Result<(), String>>,
    states: Arc<Mutex<Vec<Option<PhysicalGamepadState>>>>,
    devices: Arc<Mutex<Vec<PhysicalDeviceState>>>,
    commands: mpsc::Receiver<ProviderCommand>,
    pointer: Arc<Mutex<PointerSample>>,
    mapped_keys: Arc<Mutex<HashSet<String>>>,
    initially_enabled: bool,
}

struct SdlProviderWorker {
    _sdl: sdl3::Sdl,
    subsystem: sdl3::GamepadSubsystem,
    event_pump: sdl3::EventPump,
    remappings: RemapDatabase,
    config: ProviderConfig,
    updates: Arc<dyn GamepadUpdateSink>,
    stop: Arc<AtomicBool>,
    states: Arc<Mutex<Vec<Option<PhysicalGamepadState>>>>,
    devices: Arc<Mutex<Vec<PhysicalDeviceState>>>,
    commands: mpsc::Receiver<ProviderCommand>,
    pointer: Arc<Mutex<PointerSample>>,
    mapped_keys: Arc<Mutex<HashSet<String>>>,
    pads: Vec<Option<Pad>>,
    sequences: Vec<u32>,
    last_published: Vec<Option<GamepadState>>,
    last_connected: Vec<bool>,
    next_topology_refresh: Instant,
    next_device_snapshot: Instant,
    observed: Vec<ObservedPad>,
    source_registry: SourceRegistry,
    interactive_remappings: Vec<HashMap<BindingKey, InputBinding>>,
    interactive_mouse_mappings: Vec<HashMap<BindingKey, MouseAxisMap>>,
    keyboard_mappings: Vec<Option<CompiledKeyboardMapping>>,
    default_keyboard_mappings: Vec<Option<CompiledKeyboardMapping>>,
    pressed_keys: HashSet<String>,
    enabled: bool,
}

fn run(args: SdlWorkerArguments) {
    let started = args.started_tx.clone();
    let mut worker = match SdlProviderWorker::new(args) {
        Ok(worker) => worker,
        Err(error) => {
            let _ = started.send(Err(error));
            return;
        }
    };
    worker.update_mapped_keys();
    let _ = started.send(Ok(()));
    worker.run();
}

impl SdlProviderWorker {
    fn new(args: SdlWorkerArguments) -> Result<Self, String> {
        let (sdl, subsystem, event_pump, remappings) = initialize(&args.config)?;
        let keyboard_mappings = load_keyboard_mappings(&args.config)?;
        let slot_count = args.config.slots.len();
        Ok(Self {
            _sdl: sdl,
            subsystem,
            event_pump,
            remappings,
            config: args.config,
            updates: args.updates,
            stop: args.stop,
            states: args.states,
            devices: args.devices,
            commands: args.commands,
            pointer: args.pointer,
            mapped_keys: args.mapped_keys,
            pads: (0..slot_count).map(|_| None).collect(),
            sequences: vec![0; slot_count],
            last_published: vec![None; slot_count],
            last_connected: vec![false; slot_count],
            next_topology_refresh: Instant::now(),
            next_device_snapshot: Instant::now(),
            observed: Vec::new(),
            source_registry: SourceRegistry::default(),
            interactive_remappings: vec![HashMap::new(); slot_count],
            interactive_mouse_mappings: vec![HashMap::new(); slot_count],
            default_keyboard_mappings: keyboard_mappings.clone(),
            keyboard_mappings,
            pressed_keys: HashSet::new(),
            enabled: args.initially_enabled,
        })
    }

    fn run(&mut self) {
        while !self.stop.load(Ordering::Acquire) {
            self.poll_events();
            self.subsystem.update();
            self.drain_commands();
            self.refresh_topology();
            self.publish_samples();
            // `yield_now` immediately resumes when no equal-priority thread is
            // runnable and therefore busy-spins on Windows.
            thread::sleep(INPUT_POLL_INTERVAL);
        }
        self.shutdown();
    }

    fn poll_events(&mut self) {
        for event in self.event_pump.poll_iter().take(MAX_EVENTS_PER_POLL) {
            update_touchpad_pointer(&mut self.pads, event);
        }
    }

    fn drain_commands(&mut self) {
        let mut keyboard_bindings_changed = false;
        while let Ok(command) = self.commands.try_recv() {
            keyboard_bindings_changed |= self.handle_command(command);
        }
        if keyboard_bindings_changed {
            self.update_mapped_keys();
        }
    }

    fn handle_command(&mut self, command: ProviderCommand) -> bool {
        match command {
            ProviderCommand::SetEnabled(enabled) => {
                self.enabled = enabled;
                if !enabled {
                    self.pressed_keys.clear();
                }
                false
            }
            ProviderCommand::ReconfigureSlots { slots, response } => {
                let result = self.reconfigure(slots);
                let _ = response.send(result);
                false
            }
            ProviderCommand::SetBinding {
                slot,
                target,
                binding,
            } => {
                if let Some(bindings) = self.interactive_remappings.get_mut(slot) {
                    bindings.insert(target, binding);
                    recompile_pad(slot, &mut self.pads, &self.remappings, bindings);
                }
                false
            }
            ProviderCommand::SetKeyBinding { slot, target, key } => {
                if let Some(Some(mapping)) = self.keyboard_mappings.get_mut(slot) {
                    mapping.set(target, key);
                    return true;
                }
                false
            }
            ProviderCommand::SetMouseAxisBinding {
                slot,
                target,
                source,
                transform,
            } => {
                self.set_mouse_binding(slot, target, source, transform);
                false
            }
            ProviderCommand::ResetBindings { slot } => self.reset_bindings(slot),
            ProviderCommand::ClearBinding { slot, target } => self.clear_binding(slot, target),
            ProviderCommand::SetKey { key, pressed } => {
                if pressed {
                    self.pressed_keys.insert(key);
                } else {
                    self.pressed_keys.remove(&key);
                }
                false
            }
            ProviderCommand::ClearKeys => {
                self.pressed_keys.clear();
                false
            }
        }
    }

    fn reconfigure(&mut self, slots: Vec<SlotConfig>) -> Result<(), String> {
        if let Some(slot) = slots
            .iter()
            .find(|slot| InputLayout::get(&slot.layout_id).is_none())
        {
            return Err(format!("unknown input layout {}", slot.layout_id));
        }
        let candidate = ProviderConfig {
            slots: slots.clone(),
            gamecontroller_database: self.config.gamecontroller_database.clone(),
            keyboard_mapping_directory: self.config.keyboard_mapping_directory.clone(),
        };
        let keyboard_mappings = load_keyboard_mappings(&candidate)?;
        let old_slots = std::mem::replace(&mut self.config.slots, slots);
        let slot_count = self.config.slots.len();
        let mut pads = (0..slot_count).map(|_| None).collect::<Vec<_>>();
        let mut sequences = vec![0; slot_count];
        let mut published = vec![None; slot_count];
        let mut connected = vec![false; slot_count];
        for slot in 0..old_slots.len() {
            if self.config.slots.get(slot) == old_slots.get(slot) {
                pads[slot] = self.pads[slot].take();
                sequences[slot] = self.sequences[slot];
                published[slot] = self.last_published[slot];
                connected[slot] = self.last_connected[slot];
            } else if self.last_connected[slot] {
                self.publish_disconnected(slot);
            }
        }
        self.pads = pads;
        self.sequences = sequences;
        self.last_published = published;
        self.last_connected = connected;
        self.interactive_remappings = vec![HashMap::new(); slot_count];
        self.interactive_mouse_mappings = vec![HashMap::new(); slot_count];
        self.keyboard_mappings = keyboard_mappings;
        self.default_keyboard_mappings = self.keyboard_mappings.clone();
        self.pressed_keys.clear();
        *self.states.lock() = vec![None; slot_count];
        self.update_mapped_keys();
        self.next_topology_refresh = Instant::now();
        Ok(())
    }

    fn set_mouse_binding(
        &mut self,
        slot: usize,
        target: BindingKey,
        source: SourceControl,
        transform: AxisTransform,
    ) {
        if let Some(bindings) = self.interactive_mouse_mappings.get_mut(slot) {
            bindings.insert(target, MouseAxisMap { source, transform });
        }
        if let Some(bindings) = self.interactive_remappings.get_mut(slot) {
            bindings.insert(target, InputBinding::Disabled { disabled: true });
            recompile_pad(slot, &mut self.pads, &self.remappings, bindings);
        }
    }

    fn reset_bindings(&mut self, slot: usize) -> bool {
        if let Some(bindings) = self.interactive_remappings.get_mut(slot) {
            bindings.clear();
            recompile_pad(slot, &mut self.pads, &self.remappings, bindings);
        }
        if let Some(mapping) = self.keyboard_mappings.get_mut(slot) {
            mapping.clone_from(
                self.default_keyboard_mappings
                    .get(slot)
                    .expect("keyboard mapping vectors have equal lengths"),
            );
        }
        if let Some(bindings) = self.interactive_mouse_mappings.get_mut(slot) {
            bindings.clear();
        }
        true
    }

    fn clear_binding(&mut self, slot: usize, target: BindingKey) -> bool {
        if let Some(bindings) = self.interactive_remappings.get_mut(slot) {
            bindings.remove(&target);
            recompile_pad(slot, &mut self.pads, &self.remappings, bindings);
        }
        let defaults = self
            .default_keyboard_mappings
            .get(slot)
            .and_then(Option::as_ref);
        if let Some(Some(mapping)) = self.keyboard_mappings.get_mut(slot) {
            mapping.restore(target, defaults);
        }
        if let Some(bindings) = self.interactive_mouse_mappings.get_mut(slot) {
            bindings.remove(&target);
        }
        true
    }

    fn update_mapped_keys(&self) {
        *self.mapped_keys.lock() = self
            .keyboard_mappings
            .iter()
            .flatten()
            .flat_map(|mapping| mapping.bindings.iter().map(|(_, key, _)| key.clone()))
            .collect();
    }

    fn refresh_topology(&mut self) {
        let now = Instant::now();
        if now >= self.next_topology_refresh {
            refresh_observed_devices(
                &self.subsystem,
                &mut self.observed,
                &mut self.source_registry,
            );
            refresh_devices(
                &mut self.pads,
                &self.config.slots,
                &self.remappings,
                &self.interactive_remappings,
                &self.observed,
            );
            self.next_topology_refresh = now + Duration::from_millis(100);
        }
        if now >= self.next_device_snapshot {
            *self.devices.lock() = self.observed.iter().map(physical_device_state).collect();
            self.next_device_snapshot = now + Duration::from_millis(8);
        }
    }

    fn publish_samples(&mut self) {
        let pointer = *self.pointer.lock();
        let now = Instant::now();
        let mut snapshots = self.states.lock();
        for slot in 0..self.pads.len() {
            // Bluetooth devices are sampled by PhysicalInputRouter. Publishing
            // an SDL-owned disconnected snapshot for the same virtual slot can
            // otherwise race and neutralize the Bluetooth update.
            if matches!(
                &self.config.slots[slot].source,
                Some(SourceSelector::Device { device_id }) if device_id.starts_with("bt://")
            ) {
                snapshots[slot] = None;
                continue;
            }
            let connected = self.keyboard_mappings[slot].is_some() || self.pads[slot].is_some();
            let mut state = if let Some(mapping) = &self.keyboard_mappings[slot] {
                mapping.state(&self.pressed_keys)
            } else if let Some(pad) = &self.pads[slot] {
                let state = pad.mapping.read(&pad.gamepad);
                snapshots[slot] = Some(physical_state(pad, state));
                state
            } else {
                snapshots[slot] = None;
                GamepadState::default()
            };
            state.pointer = self.pointer_for(slot, pointer);
            if let Some(layout) = InputLayout::get(&self.config.slots[slot].layout_id) {
                apply_mouse_axes(
                    &mut state,
                    layout,
                    &self.interactive_mouse_mappings[slot],
                    pointer,
                    now,
                );
            }
            if self.keyboard_mappings[slot].is_some() {
                snapshots[slot] = Some(keyboard_diagnostic_state(
                    &self.config.slots[slot].layout_id,
                    state,
                ));
            }
            if !self.enabled {
                state = GamepadState::default();
            }
            if self.last_connected[slot] != connected || self.last_published[slot] != Some(state) {
                self.sequences[slot] = self.sequences[slot].wrapping_add(1);
                self.last_published[slot] = Some(state);
                self.last_connected[slot] = connected;
                self.updates.publish(GamepadUpdate {
                    slot: GamepadSlot::new(slot as u8).unwrap(),
                    sequence: self.sequences[slot],
                    connected,
                    state,
                });
            }
        }
    }

    fn pointer_for(&self, slot: usize, pointer: PointerSample) -> [i16; 2] {
        let configured = &self.config.slots[slot];
        if matches!(configured.source, Some(SourceSelector::KeyboardMouse))
            || configured.force_mouse
        {
            return pointer.position.unwrap_or_default();
        }
        self.pads[slot]
            .as_ref()
            .filter(|pad| pad.has_touchpad)
            .and_then(|pad| pad.touchpad_pointer)
            .or(pointer.position)
            .unwrap_or_default()
    }

    fn publish_disconnected(&mut self, slot: usize) {
        self.sequences[slot] = self.sequences[slot].wrapping_add(1);
        self.updates.publish(GamepadUpdate {
            slot: GamepadSlot::new(slot as u8).unwrap(),
            sequence: self.sequences[slot],
            connected: false,
            state: GamepadState::default(),
        });
    }

    fn shutdown(&mut self) {
        for slot in 0..self.pads.len() {
            let _ = self.pads[slot].take();
            self.publish_disconnected(slot);
            self.states.lock()[slot] = None;
        }
        self.devices.lock().clear();
    }
}

type Initialized = (
    sdl3::Sdl,
    sdl3::GamepadSubsystem,
    sdl3::EventPump,
    RemapDatabase,
);

fn initialize(config: &ProviderConfig) -> Result<Initialized, String> {
    let remappings = RemapDatabase::default();
    let sdl = sdl3::init().map_err(|error| format!("failed to initialize SDL3: {error}"))?;
    let subsystem = sdl
        .gamepad()
        .map_err(|error| format!("failed to initialize SDL3 gamepads: {error}"))?;
    for event_type in [
        EventType::JoyAxisMotion,
        EventType::JoyHatMotion,
        EventType::JoyButtonDown,
        EventType::JoyButtonUp,
        EventType::ControllerAxisMotion,
        EventType::ControllerButtonDown,
        EventType::ControllerButtonUp,
        EventType::ControllerSensorUpdated,
    ] {
        EventSubsystem::set_event_enabled(event_type, false);
    }
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
    Ok((sdl, subsystem, event_pump, remappings))
}

fn refresh_observed_devices(
    subsystem: &sdl3::GamepadSubsystem,
    observed: &mut Vec<ObservedPad>,
    registry: &mut SourceRegistry,
) {
    let Ok(ids) = subsystem.gamepads() else {
        return;
    };
    registry.observe_connections(&ids.iter().map(|id| id.0).collect());
    let mut previous = std::mem::take(observed);
    for id in ids {
        if let Some(index) = previous
            .iter()
            .position(|pad| pad.gamepad.id().is_ok_and(|existing| existing == id))
        {
            observed.push(previous.remove(index));
            continue;
        }
        match subsystem.open(id) {
            Ok(gamepad) => {
                let name = gamepad
                    .name()
                    .unwrap_or_else(|| "Unknown gamepad".to_owned());
                let fingerprint = DeviceFingerprint {
                    name: name.clone(),
                    guid: subsystem.guid_for_id(id).to_string().to_ascii_lowercase(),
                    vendor: subsystem.vendor_for_id(id),
                    product: subsystem.product_for_id(id),
                    serial: gamepad.serial_number(),
                    path: gamepad.path(),
                };
                let identity = DeviceIdentity {
                    source_id: registry.resolve(id.0, fingerprint.clone()),
                    guid: fingerprint.guid,
                    vendor: fingerprint.vendor,
                    product: fingerprint.product,
                    serial: fingerprint.serial,
                };
                observed.push(ObservedPad {
                    name,
                    gamepad: Rc::new(gamepad),
                    identity,
                });
            }
            Err(error) => {
                tracing::error!("snowglass: failed to observe SDL3 gamepad: {error}");
            }
        }
    }
}

fn physical_device_state(pad: &ObservedPad) -> PhysicalDeviceState {
    PhysicalDeviceState {
        device_id: pad.identity.source_id.clone(),
        name: pad.name.clone(),
        guid: pad.identity.guid.clone(),
        vendor: pad.identity.vendor,
        product: pad.identity.product,
        serial: pad.identity.serial.clone(),
        buttons: ALL_BUTTONS
            .into_iter()
            .map(|button| (button, pad.gamepad.button(to_sdl_button(button))))
            .collect(),
        axes: ALL_AXES
            .into_iter()
            .map(|axis| (axis, pad.gamepad.axis(to_sdl_axis(axis))))
            .collect(),
    }
}

fn refresh_devices(
    pads: &mut [Option<Pad>],
    slots: &[SlotConfig],
    remappings: &RemapDatabase,
    interactive_remappings: &[HashMap<BindingKey, InputBinding>],
    observed: &[ObservedPad],
) {
    for entry in pads.iter_mut() {
        let disconnected = entry.as_ref().is_some_and(|pad| !pad.gamepad.connected());
        if disconnected {
            let _pad = entry.take().unwrap();
        }
    }
    let mut used = pads
        .iter()
        .flatten()
        .filter_map(|pad| pad.gamepad.id().ok())
        .collect::<std::collections::HashSet<_>>();
    for slot in 0..pads.len() {
        if pads[slot].is_some() {
            continue;
        }
        let Some(id) = select_device(&used, slots[slot].source.as_ref(), observed) else {
            continue;
        };
        let Some(observed_pad) = observed
            .iter()
            .find(|pad| pad.gamepad.id().is_ok_and(|current| current == id))
        else {
            continue;
        };
        let gamepad = Rc::clone(&observed_pad.gamepad);
        let identity = observed_pad.identity.clone();
        let has_touchpad = gamepad.touchpads_count() != 0;
        let layout = InputLayout::get(&slots[slot].layout_id).expect("validated slot layout");
        enable_layout_sensors(&gamepad, layout);
        let mapping = CompiledMapping::new(
            &gamepad,
            &identity,
            layout,
            slots[slot].projection_mode,
            remappings,
            &interactive_remappings[slot],
        );
        let state = mapping.read(&gamepad);
        pads[slot] = Some(Pad {
            gamepad,
            identity,
            layout_id: slots[slot].layout_id.clone(),
            projection_mode: slots[slot].projection_mode,
            mapping,
            last: state,
            has_touchpad,
            touchpad_pointer: None,
        });
        used.insert(id);
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
    pad.touchpad_pointer = GamepadState::normalized_pointer_axes(f64::from(x), f64::from(y));
}

fn select_device(
    used: &std::collections::HashSet<sdl3::joystick::JoystickId>,
    source: Option<&SourceSelector>,
    observed: &[ObservedPad],
) -> Option<sdl3::joystick::JoystickId> {
    match source {
        Some(SourceSelector::Device { device_id }) => observed.iter().find_map(|pad| {
            (pad.identity.source_id == *device_id)
                .then(|| pad.gamepad.id().ok())
                .flatten()
                .filter(|id| !used.contains(id))
        }),
        Some(SourceSelector::KeyboardMouse) | None => None,
    }
}

fn recompile_pad(
    slot: usize,
    pads: &mut [Option<Pad>],
    remappings: &RemapDatabase,
    overrides: &HashMap<BindingKey, InputBinding>,
) {
    let Some(pad) = pads.get_mut(slot).and_then(Option::as_mut) else {
        return;
    };
    let layout = InputLayout::get(&pad.layout_id).expect("pinned layout remains available");
    pad.mapping = CompiledMapping::new(
        &pad.gamepad,
        &pad.identity,
        layout,
        pad.projection_mode,
        remappings,
        overrides,
    );
    pad.last.buttons = !pad.last.buttons;
}

fn physical_state(pad: &Pad, mapped: GamepadState) -> PhysicalGamepadState {
    PhysicalGamepadState {
        device_id: pad.identity.source_id.clone(),
        name: pad
            .gamepad
            .name()
            .unwrap_or_else(|| "Unknown gamepad".to_owned()),
        guid: pad.identity.guid.clone(),
        vendor: pad.identity.vendor,
        product: pad.identity.product,
        layout_id: pad.layout_id.clone(),
        buttons: ALL_BUTTONS
            .into_iter()
            .map(|button| (button, pad.gamepad.button(to_sdl_button(button))))
            .collect(),
        axes: ALL_AXES
            .into_iter()
            .map(|axis| (axis, pad.gamepad.axis(to_sdl_axis(axis))))
            .collect(),
        mapped_buttons: pad.mapping.mapped_buttons(),
        mapped_axes: pad.mapping.mapped_axes(),
        mapped,
    }
}

fn keyboard_diagnostic_state(layout_id: &str, mapped: GamepadState) -> PhysicalGamepadState {
    let axes = mapped.sdl_axes();
    PhysicalGamepadState {
        device_id: "keyboard-mouse".to_owned(),
        name: "Keyboard".to_owned(),
        guid: "keyboard".to_owned(),
        vendor: None,
        product: None,
        layout_id: layout_id.to_owned(),
        buttons: ALL_BUTTONS
            .iter()
            .copied()
            .enumerate()
            .map(|(index, button)| (button, mapped.buttons & (1 << index) != 0))
            .collect(),
        axes: ALL_AXES
            .iter()
            .copied()
            .enumerate()
            .map(|(index, axis)| (axis, axes[index]))
            .collect(),
        mapped_buttons: Vec::new(),
        mapped_axes: Vec::new(),
        mapped,
    }
}

impl CompiledMapping {
    fn mapped_buttons(&self) -> Vec<SdlButton> {
        let mut buttons = self
            .buttons
            .iter()
            .map(|(source, _)| *source)
            .chain(self.digital_triggers.iter().map(|(source, _)| *source))
            .collect::<Vec<_>>();
        buttons.sort_by_key(|button| *button as u8);
        buttons.dedup();
        buttons
    }

    fn mapped_axes(&self) -> Vec<SdlAxis> {
        let mut axes = self
            .axes
            .iter()
            .flatten()
            .map(|mapping| mapping.source)
            .collect::<Vec<_>>();
        axes.sort_by_key(|axis| *axis as u8);
        axes.dedup();
        axes
    }

    fn new(
        gamepad: &Gamepad,
        identity: &DeviceIdentity,
        layout: &InputLayout,
        projection_mode: ProjectionMode,
        database: &RemapDatabase,
        overrides: &HashMap<BindingKey, InputBinding>,
    ) -> Self {
        let mut targets = HashMap::<BindingKey, InputBinding>::new();
        if projection_mode != ProjectionMode::Custom {
            for button in ALL_BUTTONS {
                if let Some(target) = default_button_target_for(
                    gamepad.button_label_for_gamepad_type(to_sdl_button(button)),
                    button,
                    layout,
                    projection_mode,
                ) {
                    targets.insert(target, InputBinding::Button { button });
                }
            }
            apply_default_axes(&mut targets, layout);
            targets.extend(database.bindings.clone());
            for profile in &database.profiles {
                if profile_matches(profile, identity, layout) {
                    targets.extend(profile.bindings.clone());
                }
            }
        }
        targets.extend(overrides.clone());

        let mut buttons = Vec::new();
        let mut axes = if projection_mode == ProjectionMode::Custom {
            [None; 6]
        } else {
            DEFAULT_AXES
        };
        let mut digital_triggers = Vec::new();
        for (element, binding) in targets {
            if !layout.has(element) {
                continue;
            }
            if let Some(target) = element_button(layout, element) {
                if let InputBinding::Button { button } = binding {
                    buttons.retain(|(_, existing)| *existing != target);
                    buttons.push((button, target));
                }
                continue;
            }
            let Some(index) = element_axis_index(layout, element) else {
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
            accelerometer: layout.has_analog(AnalogClass::Accelerometer),
            gyroscope: layout.has_analog(AnalogClass::Gyroscope),
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

fn enable_layout_sensors(gamepad: &Gamepad, layout: &InputLayout) {
    for (enabled, sensor) in [
        (
            layout.has_analog(AnalogClass::Accelerometer),
            SensorType::Accelerometer,
        ),
        (
            layout.has_analog(AnalogClass::Gyroscope),
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

/// The device control the virtual pad presents at a wire location.
///
/// This is the projection read backwards. The pad advertises a GameCube's Z at
/// the right shoulder, so the physical right shoulder is what should drive it —
/// and a hand-written table of the same thing could only ever be a second,
/// disagreeing opinion.
fn projected_from(layout: &InputLayout, wire: Wire) -> Option<BindingKey> {
    layout
        .elements
        .iter()
        .map(|element| element.key)
        .find(|key| layout.sdl_wire(*key) == Some(wire))
}

/// The device control a *physically labelled* face button should drive.
///
/// The one question the projection cannot answer: which button a user will
/// press for "A" depends on what their hardware prints, not on where the pad
/// puts it. A Nintendo pad prints A on the east position, so east is what
/// should drive the emulated A.
fn labelled_face_target_for(label: ButtonLabel, layout: &InputLayout) -> Option<BindingKey> {
    let label = match label {
        ButtonLabel::A => "A",
        ButtonLabel::B => "B",
        ButtonLabel::X => "X",
        ButtonLabel::Y => "Y",
        ButtonLabel::Cross => "Cross",
        ButtonLabel::Circle => "Circle",
        ButtonLabel::Square => "Square",
        ButtonLabel::Triangle => "Triangle",
        ButtonLabel::Unknown => return None,
    };
    layout
        .elements
        .iter()
        .find(|element| element.label.eq_ignore_ascii_case(label))
        .map(|element| element.key)
}

fn default_button_target_for(
    label: ButtonLabel,
    source: SdlButton,
    layout: &InputLayout,
    projection_mode: ProjectionMode,
) -> Option<BindingKey> {
    if projection_mode == ProjectionMode::Name
        && matches!(
            source,
            SdlButton::South | SdlButton::East | SdlButton::West | SdlButton::North
        )
        && let Some(target) = labelled_face_target_for(label, layout)
    {
        return Some(target);
    }
    projected_from(layout, Wire::Button(source)).or_else(|| positional_face_target(source, layout))
}

fn positional_face_target(source: SdlButton, layout: &InputLayout) -> Option<BindingKey> {
    let element = match source {
        SdlButton::South => ButtonElement::A,
        SdlButton::East => ButtonElement::B,
        SdlButton::West => ButtonElement::X,
        SdlButton::North => ButtonElement::Y,
        _ => return None,
    };
    layout.resolve(BindingKey::Button(element))
}

fn apply_default_axes(targets: &mut HashMap<BindingKey, InputBinding>, layout: &InputLayout) {
    // A dial declares no side, so it answers to neither stick's key; it is left
    // unaligned and reads as the primary analog axis, as an SDL paddle does.
    let sticks = [
        (
            AnalogClass::Stick,
            Some(Alignment::Left),
            SdlAxis::LeftX,
            SdlAxis::LeftY,
        ),
        (
            AnalogClass::Stick,
            Some(Alignment::Right),
            SdlAxis::RightX,
            SdlAxis::RightY,
        ),
        (AnalogClass::Rotary, None, SdlAxis::LeftX, SdlAxis::LeftY),
    ];
    let mut bind = |key, axis| {
        if layout.has(key) {
            targets.insert(
                key,
                InputBinding::Axis {
                    axis,
                    invert: false,
                },
            );
        }
    };
    for (class, alignment, x, y) in sticks {
        for (axis, source) in [(Axis::X, x), (Axis::Y, y)] {
            for sign in [Sign::Positive, Sign::Negative] {
                bind(
                    BindingKey::Analog {
                        class,
                        alignment,
                        component: Component::new(axis, sign),
                    },
                    source,
                );
            }
        }
    }
    for (alignment, source) in [
        (Alignment::Left, SdlAxis::LeftTrigger),
        (Alignment::Right, SdlAxis::RightTrigger),
    ] {
        bind(BindingKey::Trigger { alignment }, source);
    }
}

fn profile_matches(
    profile: &RemapProfile,
    identity: &DeviceIdentity,
    layout: &InputLayout,
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

fn element_button(layout: &InputLayout, key: BindingKey) -> Option<GamepadButton> {
    match layout.sdl_wire(key)? {
        Wire::Button(button) => GamepadButton::from_wire(button.index()),
        _ => None,
    }
}

/// Which of the six gamepad axes a key drives: 0/1 the left stick, 2/3 the
/// right, 4/5 the triggers. The pointer's raw axes are outside the gamepad
/// mapping and are not one of them.
fn element_axis_index(layout: &InputLayout, key: BindingKey) -> Option<usize> {
    match layout.sdl_wire(key)? {
        // The pointer's raw axes are outside the gamepad mapping, and
        // `Wire::RawAxis` is how they say so.
        Wire::Axis(axis) => Some(usize::from(axis.index())),
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

fn to_sdl_axis(axis: SdlAxis) -> SdlNativeAxis {
    match axis {
        SdlAxis::LeftX => SdlNativeAxis::LeftX,
        SdlAxis::LeftY => SdlNativeAxis::LeftY,
        SdlAxis::RightX => SdlNativeAxis::RightX,
        SdlAxis::RightY => SdlNativeAxis::RightY,
        SdlAxis::LeftTrigger => SdlNativeAxis::TriggerLeft,
        SdlAxis::RightTrigger => SdlNativeAxis::TriggerRight,
    }
}

fn load_keyboard_mappings(
    config: &ProviderConfig,
) -> Result<Vec<Option<CompiledKeyboardMapping>>, String> {
    let directory = config
        .keyboard_mapping_directory
        .clone()
        .unwrap_or_else(|| PathBuf::from("."));
    let disk_default = directory.join("DEFAULT.kb.toml");
    let default_toml = if disk_default.is_file() {
        fs::read_to_string(&disk_default)
            .map_err(|error| format!("failed to read {}: {error}", disk_default.display()))?
    } else {
        EMBEDDED_DEFAULT_KEYBOARD_MAPPING.to_owned()
    };

    config
        .slots
        .iter()
        .map(|slot| {
            if !matches!(slot.source, Some(SourceSelector::KeyboardMouse)) {
                return Ok(None);
            }
            let layout = InputLayout::get(&slot.layout_id).expect("slot layout was validated");
            if slot.projection_mode == ProjectionMode::Custom {
                return CompiledKeyboardMapping::new(KeyboardMapping::default(), layout).map(Some);
            }
            let controller_path = directory.join(format!("{}.kb.toml", slot.layout_id));
            let source = if controller_path.is_file() {
                fs::read_to_string(&controller_path).map_err(|error| {
                    format!("failed to read {}: {error}", controller_path.display())
                })?
            } else {
                default_toml.clone()
            };
            let mapping = KeyboardMapping::from_toml(&source)
                .map_err(|error| format!("{}: {error}", controller_path.display()))?;
            CompiledKeyboardMapping::new(mapping, layout).map(Some)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::Projection;
    use consolespec::input::key;

    fn face_order(
        layout: &InputLayout,
        mode: ProjectionMode,
        labels: [ButtonLabel; 4],
    ) -> Vec<&str> {
        [
            SdlButton::South,
            SdlButton::East,
            SdlButton::West,
            SdlButton::North,
        ]
        .into_iter()
        .zip(labels)
        .map(|(source, label)| {
            let target = default_button_target_for(label, source, layout, mode).unwrap();
            layout
                .elements
                .iter()
                .find(|element| element.key == target)
                .unwrap()
                .label
                .as_str()
        })
        .collect()
    }

    #[test]
    fn nintendo_face_buttons_can_follow_names_or_positions() {
        let layout = InputLayout::get("SWITCH_PRO_CONTROLLER").unwrap();
        let labels = [
            ButtonLabel::B,
            ButtonLabel::A,
            ButtonLabel::Y,
            ButtonLabel::X,
        ];
        assert_eq!(
            face_order(layout, ProjectionMode::Name, labels),
            ["B", "A", "Y", "X"]
        );
        assert_eq!(
            face_order(layout, ProjectionMode::Position, labels),
            ["A", "B", "X", "Y"]
        );
    }

    #[test]
    fn playstation_face_buttons_keep_cross_circle_square_triangle_order() {
        let layout = InputLayout::get("DUALSENSE_CONTROLLER").unwrap();
        let labels = [
            ButtonLabel::Cross,
            ButtonLabel::Circle,
            ButtonLabel::Square,
            ButtonLabel::Triangle,
        ];
        let expected = ["Cross", "Circle", "Square", "Triangle"];
        assert_eq!(face_order(layout, ProjectionMode::Name, labels), expected);
        assert_eq!(
            face_order(layout, ProjectionMode::Position, labels),
            expected
        );
    }

    /// The provider's own table could only answer for controls whose name the
    /// pad already had, so every remapped control fell through it and reached
    /// nothing. Asking the projection is what fixes that, and these are the
    /// three shapes it used to get wrong.
    #[test]
    fn remapped_controls_reach_the_wire_the_pad_presents() {
        let gamecube = InputLayout::get("GAMECUBE_CONTROLLER").unwrap();
        // Z has no name on the pad; the projection puts it on the right shoulder.
        assert_eq!(
            element_button(gamecube, key!(button.z)),
            Some(GamepadButton::R)
        );
        // The digital shoulder clicks drive the trigger axes.
        assert_eq!(element_axis_index(gamecube, key!(button.l)), Some(4));

        // An N64 C-button is a directional that drives half of the right stick.
        let n64 = InputLayout::get("N64_CONTROLLER").unwrap();
        assert_eq!(element_axis_index(n64, key!(directional.right.n)), Some(3));
        // Its D-pad still reaches the hat.
        assert_eq!(
            element_button(n64, key!(directional.left.n)),
            Some(GamepadButton::DpadUp)
        );

        // A keypad button is routed off the pad, so it drives no wire at all.
        let a5200 = InputLayout::get("A5200_CONTROLLER").unwrap();
        assert_eq!(element_button(a5200, key!(button.0)), None);
        assert_eq!(element_axis_index(a5200, key!(button.0)), None);
    }

    /// The Wii pointer is projected onto the right stick, while the Nunchuk
    /// keeps the left stick for its own analog control.
    #[test]
    fn wii_pointer_and_nunchuk_stick_use_separate_gamepad_axes() {
        let remote = InputLayout::get("WII_REMOTE_CONTROLLER").unwrap();
        assert_eq!(element_axis_index(remote, key!(pointer.x+)), Some(2));
        assert_eq!(element_axis_index(remote, key!(pointer.y+)), Some(3));

        let nunchuk = InputLayout::get("WII_REMOTE_NUNCHUCK_CONTROLLER").unwrap();
        assert_eq!(
            element_axis_index(nunchuk, key!(analog.stick.left.x+)),
            Some(0)
        );
        assert_eq!(element_axis_index(nunchuk, key!(pointer.x+)), Some(2));
    }

    /// Physical and virtual now agree by construction: whatever the pad
    /// advertises at a wire location is what the physical button there drives.
    /// A hand-written table could not state this, let alone be checked against it.
    #[test]
    fn every_advertised_button_round_trips_through_the_wire() {
        let projection = Projection::shared();
        for layout in InputLayout::all() {
            let mask = layout.sdl_output(projection).button_mask;
            for (sdl, _) in crate::sdl::BUTTONS {
                let advertised = mask & (1 << sdl.index()) != 0;
                let Some(key) = projected_from(layout, Wire::Button(sdl)) else {
                    assert!(
                        !advertised,
                        "{}: {sdl:?} is advertised but nothing drives it",
                        layout.id
                    );
                    continue;
                };
                assert!(
                    advertised,
                    "{}: {key} drives {sdl:?}, which is not advertised",
                    layout.id
                );
                assert_eq!(
                    layout.sdl_wire(key),
                    Some(Wire::Button(sdl)),
                    "{}: {key}",
                    layout.id
                );
            }
        }
    }

    /// The inverse takes the first control it finds, so two controls sharing a
    /// wire button would make it arbitrary. None do.
    #[test]
    fn no_two_controls_share_a_wire_button() {
        for layout in InputLayout::all() {
            let mut seen: HashMap<SdlButton, BindingKey> = HashMap::new();
            for element in &layout.elements {
                if let Some(Wire::Button(wire)) = layout.sdl_wire(element.key)
                    && let Some(other) = seen.insert(wire, element.key)
                {
                    panic!(
                        "{}: {other} and {} both drive {wire:?}",
                        layout.id, element.key
                    );
                }
            }
        }
    }

    /// The inverse reaches controls the old fixed table had no name for.
    #[test]
    fn the_inverse_finds_what_a_fixed_table_could_not() {
        let gamecube = InputLayout::get("GAMECUBE_CONTROLLER").unwrap();
        // Right shoulder is where the pad advertises Z, so that is what it drives.
        assert_eq!(
            projected_from(gamecube, Wire::Button(SdlButton::RightShoulder)),
            Some(key!(button.z))
        );
        // And nothing is advertised at the left shoulder, so nothing drives it.
        assert_eq!(
            projected_from(gamecube, Wire::Button(SdlButton::LeftShoulder)),
            None
        );

        // The N64's D-pad holds the hat; its C-buttons are on the right stick
        // and so are not reachable from any physical button.
        let n64 = InputLayout::get("N64_CONTROLLER").unwrap();
        assert_eq!(
            projected_from(n64, Wire::Button(SdlButton::DpadUp)),
            Some(key!(directional.left.n))
        );
    }

    #[test]
    fn trigger_conversion_covers_the_wire_range() {
        assert_eq!(trigger(i16::MIN), 0);
        assert_eq!(trigger(0), 0);
        assert_eq!(trigger(i16::MAX), u16::MAX);
    }

    #[test]
    fn touchpad_coordinates_fill_pointer_axes() {
        assert_eq!(
            GamepadState::normalized_pointer_axes(0.0, 0.0),
            Some([i16::MIN, i16::MIN])
        );
        assert_eq!(
            GamepadState::normalized_pointer_axes(0.5, 0.5),
            Some([0, 0])
        );
        assert_eq!(
            GamepadState::normalized_pointer_axes(1.0, 1.0),
            Some([i16::MAX, i16::MAX])
        );
    }

    #[test]
    fn embedded_keyboard_mapping_drives_wii_remote_buttons() {
        let layout = InputLayout::get("WII_REMOTE_CONTROLLER").unwrap();
        let mapping = KeyboardMapping::from_toml(EMBEDDED_DEFAULT_KEYBOARD_MAPPING).unwrap();
        let mapping = CompiledKeyboardMapping::new(mapping, layout).unwrap();
        let pressed = HashSet::from(["KeyX".to_owned(), "ArrowUp".to_owned()]);
        let state = mapping.state(&pressed);
        assert_ne!(state.buttons & GamepadButton::A.mask(), 0);
        assert_ne!(state.buttons & GamepadButton::DpadUp.mask(), 0);
        let diagnostic = keyboard_diagnostic_state("WII_REMOTE_CONTROLLER", state);
        assert_eq!(diagnostic.name, "Keyboard");
        assert_eq!(diagnostic.mapped, state);
        assert!(diagnostic.buttons.contains(&(SdlButton::South, true)));
    }

    #[test]
    fn keyboard_targets_can_be_overridden_and_restored_live() {
        let layout = InputLayout::get("XBOX_CONTROLLER").unwrap();
        let source = KeyboardMapping::from_toml(EMBEDDED_DEFAULT_KEYBOARD_MAPPING).unwrap();
        let defaults = CompiledKeyboardMapping::new(source, layout).unwrap();
        let mut mapping = defaults.clone();

        mapping.set(key!(button.a), Some("KeyZ".to_owned()));
        assert_eq!(
            mapping.state(&HashSet::from(["KeyX".to_owned()])).buttons & GamepadButton::A.mask(),
            0
        );
        assert_ne!(
            mapping.state(&HashSet::from(["KeyZ".to_owned()])).buttons & GamepadButton::A.mask(),
            0
        );

        mapping.restore(key!(button.a), Some(&defaults));
        assert_ne!(
            mapping.state(&HashSet::from(["KeyX".to_owned()])).buttons & GamepadButton::A.mask(),
            0
        );
    }

    #[test]
    fn mouse_delta_drives_an_axis_then_decays_to_neutral() {
        let now = Instant::now();
        let mut pointer = PointerSample::default();
        pointer.set_relative([50.0, 0.0], now);
        let mappings = HashMap::from([(
            key!(analog.stick.right.x+),
            MouseAxisMap {
                source: SourceControl::MouseDelta {
                    axis: PointerAxis::X,
                },
                transform: AxisTransform {
                    scale: 2.0,
                    ..AxisTransform::default()
                },
            },
        )]);

        let mut state = GamepadState::default();
        let layout = InputLayout::get("XBOX_CONTROLLER").unwrap();
        apply_mouse_axes(&mut state, layout, &mappings, pointer, now);
        assert_eq!(state.right_stick[0], i16::MAX);

        apply_mouse_axes(
            &mut state,
            layout,
            &mappings,
            pointer,
            now + RELATIVE_MOUSE_RELEASE,
        );
        assert_eq!(state.right_stick[0], 0);
    }

    #[test]
    fn mouse_position_applies_axis_transform() {
        let now = Instant::now();
        let mut pointer = PointerSample::default();
        pointer.set_position(Some([i16::MAX, 0]));
        let mappings = HashMap::from([(
            key!(analog.stick.left.x+),
            MouseAxisMap {
                source: SourceControl::MousePosition {
                    axis: PointerAxis::X,
                },
                transform: AxisTransform {
                    invert: true,
                    scale: 0.5,
                    dead_zone: 0.0,
                },
            },
        )]);
        let mut state = GamepadState::default();
        let layout = InputLayout::get("XBOX_CONTROLLER").unwrap();
        apply_mouse_axes(&mut state, layout, &mappings, pointer, now);
        assert_eq!(state.left_stick[0], -16_384);
    }

    #[test]
    fn a_target_the_grammar_does_not_admit_fails_the_document() {
        // Not a layout question but a syntax one, so it is caught at parse and
        // names the offending key rather than being dropped silently later.
        let error = RemapDatabase::from_json(
            r#"{"bindings":{"DefinitelyNotAInputElement":{"button":"south"}}}"#,
        )
        .unwrap_err();
        assert!(error.contains("DefinitelyNotAInputElement"), "{error}");
    }

    #[test]
    fn provider_initializes_without_a_connected_gamepad() {
        let mailbox = Arc::new(LatestGamepadMailbox::new(1));
        let provider = Sdl3Provider::start(ProviderConfig::default(), mailbox).unwrap();
        provider.reconfigure_slots(Vec::new()).unwrap();
        assert!(provider.states().is_empty());
        drop(provider);
    }

    #[test]
    fn discovery_provider_accepts_an_empty_virtual_topology() {
        assert!(
            validate_config(&ProviderConfig {
                slots: Vec::new(),
                ..ProviderConfig::default()
            })
            .is_ok()
        );
    }
}
