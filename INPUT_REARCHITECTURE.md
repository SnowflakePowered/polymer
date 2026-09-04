# Input subsystem rearchitecture

Target audience: the agent implementing this. Read the whole document before
touching code. This is a clean API break — no compatibility shims, no
deprecated re-exports, no legacy wire messages.

Repo conventions that apply (from `AGENTS.md` and standing user preferences):

- `mod.rs` module style, functions bound as trait/struct impls rather than free
  functions, low cyclomatic complexity, `zerocopy` for binary parsing.
- **Write bare code. No `//` or `///` comments.** Reasoning belongs in the PR
  description / reply, not the source. The existing input crates are heavily
  commented; the rewritten modules must not be.
- **Do not add tests without asking.** Section 17 proposes coverage; get sign-off
  before writing any of it.
- Protocol version bumps require a full workspace rebuild and killing leftover
  `polymer-viewer.exe` / target processes before re-running.

## Settled decisions

Answered; no longer open. Recorded here because several differ from the
recommendation in the section that raised them, and the section text has been
updated to match.

| # | Question | Decision |
| --- | --- | --- |
| 1 | Crate names (§14.12) | As proposed. The router takes the name `InputHub`; the old type becomes `polymer-controllers::ControllerConfiguration`. Host SDL capture is `polymer-input-sdl3`, target SDL injection is `polymer-hook-input-sdl` — the `hook-` prefix and the `crates/hooks/` location disambiguate. |
| 2 | Feedback arbitration (§14.3) | Single owner. |
| 3 | `PointerCapabilities` split (§15.13) | **Merged.** One `Capabilities` bitfield covers general, pointer, and calibration bits. `PointerCapabilities` and `CalibrationFields` are deleted. |
| 4 | Index range (§14.8) | `max_players` = 8, `u8` wire indices. |
| 5 | Topology encoding (§14.13) | **`bincode`**, not `serde_json`. |
| 6 | Who knows a key is consumed (§14.1) | By construction — the GUI compiles the same `Mapping` it publishes. |
| 7 | Where `PointerBinding` lives (§15.13) | **Per-port configuration**, not a separate `pointers: []` on the launch result. |
| 8 | Default `PointerMode` (§15.13) | `Relative`. |
| 9 | Factory calibration defaults (§16.9) | Per profile, **keyed by `(vendor_id, product_id)`**. On-board factory config gets read eventually; the defaults are the interim value behind the same interface. |
| 10 | Persist runtime gyro bias (§16.9) | Yes, `MOTION_BIAS` only, with a visible reset. |
| 11 | Composite persistence (§14.6) | **Not persisted, but deterministically rebuilt.** Compositions and indices reset on app restart and are re-established for bonded devices by background reconnect, with no `ConnectionSession`. A session is required only for fresh bonding. |

Three of these change the shape of the design rather than confirming a default —
3, 7, and 11. Their sections are rewritten below; 11 in particular adds
behaviour that did not previously exist.

---

## 1. Goal

`polymer-input` becomes an **input hub and router**: it owns physical device
discovery aggregation, stable player-index assignment, device composition, and
fan-out of gamepad state / raw HID to listeners. It republishes state *as the
device reports it*. It knows nothing about consolespec inputspecs, emulator
ports, layouts, projections, or user remapping.

Everything below the hub is a **HAL backend** (`InputBackend`). Everything above
is a **listener** (`InputListener` / `HostInputListener`).

Remapping moves into the injected hooks, driven by a runtime-side control
channel that publishes layout + binding revisions, mirroring how graphics IPC
publishes stream topology.

---

## 2. What is wrong today

Concrete, with file references, so the rewrite can be checked against it.

### 2.1 `polymer-input` has at least six unrelated responsibilities

| Module | Responsibility | Belongs |
| --- | --- | --- |
| `src/hub.rs` (1107 lines) | machinespec port topology, revisioned transactions, binding overrides | host-side controller config crate |
| `src/topology/mod.rs` (483) | emulator port ↔ virtual pad mapping, plugin-facing JSON | host-side controller config crate |
| `src/layout.rs` (672) | consolespec inputspec → SDL pad projection, keyboard mapping docs | hook-safe layout crate |
| `src/sdl.rs` (374) | the SDL virtual pad as a device | hook-safe layout crate |
| `src/provider/sdl3/mod.rs` (2122) | SDL capture **and** mapping compilation **and** keyboard projection **and** pointer synthesis | split: capture → HAL crate, rest → hooks |
| `src/router/mod.rs` (420) | physical→slot routing **and** button/axis remapping | hub keeps routing, drops remapping |
| `src/mouse/layout.rs` (404) | letterbox/aspect-fit compositing geometry — **not input at all** | `polymer-renderer` |
| `src/mouse/mod.rs` | `PointerInput` wire struct | `polymer-protocol` |
| `src/lib.rs` | `DirectInputData`, `RawKeyInput` wire structs | `polymer-protocol` + dinput hook |
| `build.rs` | generates `SDL3.lib` from the hook's `SDL3.dll` | SDL HAL crate only |

The `build.rs` is the sharpest symptom: **every crate that depends on
`polymer-input` — including four injected hook DLLs — drags in an SDL3 import
library and a `lib.exe` invocation** it has no use for.

### 2.2 Duplicated representations of one mapping

The same user binding exists as five types:

1. `topology::ControllerBindings` — `HashMap<BindingKey, Option<BindingKey>>`, plugin JSON.
2. `hub::BindingOverride` — tagged enum with `SourceControl` + `AxisTransform`.
3. `layout::InputBinding` — `Button` / `Axis` / `Disabled`.
4. `router::GamepadRemap` — `Vec<(u8,u8)>` + `Vec<(u8,u8,bool)>` wire indices.
5. `provider::sdl3::CompiledMapping` / `CompiledKeyboardMapping` — runtime form.

A single binding change walks 1→2→3→5 on the reconcile path
(`bins/polymer-gui/src/input/reconcile.rs`) and 1→2→4 on the Bluetooth path
(`ControllerManager::routes_for`). `ResolvedInputConfiguration` exists purely to
carry (2) alongside (1).

### 2.3 Per-report allocation and cloning in the hot path

- `polymer-ipc/src/control.rs:1014` — `encode_input` does
  `encode(value).as_bytes().to_vec()`: **a heap allocation per gamepad update**,
  ~1 kB (`PAYLOAD_SIZE` is sized by `WireStreamReady`), for a ~40-byte payload.
- `polymer-input-bluetooth/src/lib.rs` — every HID report clones the whole
  `BluetoothControllerState` (name, profile id, motion frame, …) *and*
  `broadcast::send`s a `Box` of it, then separately publishes to the router.
- `bins/polymer-gui/src/input/publication/mod.rs` rebuilds a
  `serde_json::Value` device tree at 60 Hz (`publication_due`, 16 ms).
- `Sdl3Provider::states()` / `devices()` clone `Vec<PhysicalGamepadState>` out of
  a mutex on every poll.
- `RoutedGamepadSink` (GUI) re-sequences and re-buffers everything the router
  already sequenced — two sequence counters for one stream.

### 2.4 Index assignment is split across four owners

`ControllerManager.players` (runtime), `InputHub.assignments` (input),
`PhysicalInputRouter.routes` (input), `Sdl3Provider.slots` (input). Assigning a
pad runs a transaction through all four, and `PortConfiguration::slot` exists to
paper over the skew between emulator ports and virtual pads.

### 2.5 Raw HID is Wii-Remote- and Dolphin-specific, and bypasses the hub

`polymer-input-bluetooth/src/raw/mod.rs` creates named pipes directly against
`BluetoothManager`, applies Dolphin-specific report filtering and a
speaker-report→rumble rewrite, and is configured out of band via
`POLYMER_BLUETOOTH_HID_PIPES`. The hub never sees it.

### 2.6 Layout topology is frozen at launch

`polymer-runtime/src/launch.rs:85` writes `POLYMER_LAYOUTS` and
`POLYMER_SDL_GAMEPADS` into the target environment. The SDL hooks read them
once (`polymer-hook-sdl3/src/install.rs::create_table_state`). A controller
change after launch (`onControllerChanged`) cannot alter the virtual pad
topology, only the values flowing through it.

---

## 3. Target crate graph

```
                      consolespec        polymer-config
                            \                  /
                             \                /
                        polymer-input-layout          (hook-safe: layouts,
                           /        |        \           SDL pad, projection,
                          /         |         \          mapping application)
                         /          |          \
       polymer-controllers   hook DLLs   polymer-input-ipc
       (host: ports, topology,     |                |
        profiles, persistence)     |                |
                    \              |                |
                     \             |                |
                      +---> polymer-input <-------+       (HUB + ROUTER)
                            /            \
                           /              \
             polymer-input-sdl3    polymer-input-bluetooth   (HAL backends)
                                              |
                                       polymer-bluetooth       (radio HAL)
```

`polymer-input` depends on: `parking_lot`, `tracing`. **Nothing else.** No
serde, no serde_json, no consolespec, no SDL, no IPC.

### 3.1 New crates

| Crate | Path | Role |
| --- | --- | --- |
| `polymer-input-layout` | `crates/input/polymer-input-layout` | Hook-safe. `InputLayout`, `SdlButton`/`SdlAxis`/`Wire`/`SdlSensor`, projection re-export, `Mapping` application, keyboard mapping documents. Deps: `consolespec`, `polymer-config`, `polymer-input`, `serde`, `toml`. **No SDL, no IPC.** |
| `polymer-input-sdl3` | `crates/input/polymer-input-sdl3` | Host-side SDL3 HAL backend. Owns the `sdl3` dependency and the `SDL3.lib` `build.rs`. |
| `polymer-input-ipc` | `crates/input/polymer-input-ipc` | The IPC listener. Owns the shared-memory publisher, the target control socket, the raw-HID duplex channel, and the topology control channel. |
| `polymer-controllers` | `crates/runtime/polymer-controllers` | Host-side emulator controller topology, user configuration, mapping profile persistence. Absorbs today's `polymer-input::hub`, `::topology`, and `polymer-runtime::controllers`. |
| `polymer-hook-input` | `crates/hooks/polymer-hook-input` | Shared target-side runtime: IPC subscriber, latest-state store, keyboard/mouse queues, mapping application via `polymer-input-layout`, feedback publish. |

### 3.2 Renamed / relocated crates

| From | To | Note |
| --- | --- | --- |
| `crates/input/polymer-input-sdl` | `crates/hooks/polymer-hook-input-sdl` | Target-side only. `SdlBackend` trait + virtual pad attach/detach/rebind; state store moves to `polymer-hook-input`. |
| `crates/input/polymer-input-win32` | `crates/hooks/polymer-hook-input-win32` | Target-side only. |
| `crates/input/polymer-input-dinput` | `crates/hooks/polymer-hook-input-dinput` | **Target half only.** `DirectInputForwarder` and `RawKeyboardForwarder` are deleted — see §10. |

After this, `crates/input/` contains only host-side crates:
`polymer-bluetooth`, `polymer-input`, `polymer-input-layout`,
`polymer-input-bluetooth`, `polymer-input-sdl3`, `polymer-input-ipc`.

### 3.3 Deleted outright

- `polymer-input/src/router/mod.rs::GamepadRemap` — remapping is no longer a
  hub concern.
- `polymer-input/src/provider/sdl3/mod.rs` mapping machinery: `CompiledMapping`,
  `CompiledKeyboardMapping`, `KeyboardTarget`, `MouseAxisMap`, `AxisMap`,
  `apply_mouse_axes`, `transform_axis`, `keyboard_target`, `projected_from`,
  `labelled_face_target_for`, `default_button_target_for`,
  `positional_face_target`, `apply_default_axes`, `profile_matches`,
  `element_button`, `element_axis_index`, `load_keyboard_mappings`,
  `DEFAULT_AXES`. Roughly 1100 of its 2122 lines move to
  `polymer-input-layout::mapping`.
- `crates/runtime/polymer-runtime/src/input.rs` — replaced by
  `polymer-input-ipc`.
- `bins/polymer-gui/src/input/reconcile.rs` — reconciliation collapses to
  `hub.assign()` plus a topology-revision publish.
- `bins/polymer-gui/src/input.rs::RoutedGamepadSink` and `RoutedInputSender` —
  the hub owns fan-out and replay; attach/detach becomes listener registration.
- `polymer-runtime::ResolvedInputConfiguration` / `ResolvedSlotMapping`.
- `polymer-input-dinput::DirectInputForwarder`, `::RawKeyboardForwarder`.
- `polymer-hook::config::{LAYOUT, LAYOUTS, SDL_GAMEPADS, BLUETOOTH_HID_PIPES}`
  and their accessors — replaced by the control channel.
- `polymer-input::gamepad::LatestGamepadMailbox` — the hub's per-index slot
  already coalesces; the mailbox's wake channel moves into the IPC publisher.

### 3.4 Moved out of the input tree entirely

- `polymer-input::mouse::layout` (`Extent`, `Rect`, `Point`, `AffineMapping`,
  `SurfaceLayout`, `CompositePlacement`, `aspect_fit`,
  `vertical_surface_layout`) → `crates/consumer/polymer-renderer/src/layout/mod.rs`.
  Consumers to update: `polymer-renderer/src/{lib,input,renderer/mod,renderer/d3d12/raster_surface}.rs`,
  `bins/polymer-debug-viewer/src/layout.rs`, `bins/polymer-gui/src/game_host/mod.rs`.
- `polymer-input::mouse::{PointerInput, PointerPhase, PointerSource}` and
  `polymer-input::{DirectInputData, DirectInputDevice, RawKeyInput}` →
  `polymer-protocol`. They are wire structs; `polymer-ipc` already treats
  them as such, and moving them removes `polymer-ipc`'s dependency on the hub.

---

## 4. `polymer-input` — the hub

New module layout:

```
crates/input/polymer-input/src/
  lib.rs
  device/mod.rs        DeviceId, DeviceDescriptor, DeviceRole, Capabilities, BatteryState
  report/mod.rs        GamepadButton, GamepadReport, SensorReport, PointerReport, DeviceState
  feedback/mod.rs      Feedback, FeedbackClass, HdRumbleFrame, HdRumbleStream, LedPattern
  calibration/mod.rs   Calibration, CalibrationCapture, PointerSetup
  backend/mod.rs       InputBackend, BackendHandle, BackendId, BackendError, RawHidFilter
  listener/mod.rs      InputListener, HostInputListener, ListenerHandle, InputEvent, HostInputEvent
  hub/mod.rs           InputHub, AssignmentPolicy, CompositionMode, HubError
  hub/registry.rs      device registry, retention of disconnected devices
  hub/assignment.rs    player index allocation, composition
```

### 4.1 Identity and description

```rust
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DeviceId(Arc<str>);

impl DeviceId {
    pub fn new(backend: BackendId, key: &str) -> Self;
    pub fn backend(&self) -> BackendId;
    pub fn as_str(&self) -> &str;
}

#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BackendId(&'static str);

impl BackendId {
    pub const SDL3: Self = Self("sdl3");
    pub const BLUETOOTH: Self = Self("bt");
    pub const VIRTUAL: Self = Self("virtual");
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub struct PlayerIndex(u8);

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum DeviceRole { Whole, LeftHalf, RightHalf }

bitflags! {
    pub struct Capabilities: u32 {
        const RUMBLE                 = 1 << 0;
        const HD_RUMBLE              = 1 << 1;
        const PLAYER_LED             = 1 << 2;
        const HOME_LED               = 1 << 3;
        const ACCELEROMETER          = 1 << 4;
        const GYROSCOPE              = 1 << 5;
        const ORIENTATION            = 1 << 6;
        const TOUCHPAD               = 1 << 7;
        const BATTERY                = 1 << 8;
        const RAW_HID                = 1 << 9;
        const RECENTER               = 1 << 10;

        const POINTER_RELATIVE       = 1 << 11;
        const POINTER_ANGULAR        = 1 << 12;
        const POINTER_SURFACE        = 1 << 13;
        const POINTER_CONTACT        = 1 << 14;

        const CALIBRATE_STICK_CENTER = 1 << 16;
        const CALIBRATE_STICK_RANGE  = 1 << 17;
        const CALIBRATE_DEAD_ZONE    = 1 << 18;
        const CALIBRATE_TRIGGER      = 1 << 19;
        const CALIBRATE_MOTION_BIAS  = 1 << 20;
        const CALIBRATE_MOTION_SCALE = 1 << 21;
        const CALIBRATE_POINTER      = 1 << 22;
    }
}

impl Capabilities {
    pub const POINTER_ABSOLUTE: Self =
        Self::POINTER_ANGULAR.union(Self::POINTER_SURFACE);
    pub const POINTER: Self =
        Self::POINTER_RELATIVE.union(Self::POINTER_ABSOLUTE);

    pub const fn has_pointer(self) -> bool { self.intersects(Self::POINTER) }
}

#[derive(Clone)]
pub struct DeviceDescriptor {
    pub id: DeviceId,
    pub name: Arc<str>,
    pub profile: Option<Arc<str>>,
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
    pub serial: Option<Arc<str>>,
    pub role: DeviceRole,
    pub capabilities: Capabilities,
}
```

One bitfield, three groups. The `POINTER_*` bits say which modalities a device
can serve, and are what a plugin reads to choose a `PointerMode` (§15.4). The
`CALIBRATE_*` bits say which calibration a GUI may offer (§16.4) — capabilities
rather than a read method, because calibration is write-only at the HAL
boundary. There is no standalone `POINTER` bit: it is derived, since "has a
pointer" is exactly "serves some pointer modality".

`DeviceDescriptor` carries **no `serde_json::Value` metadata bag**. The fields
the GUI and the Dolphin plugin actually read today (`profileId`, `vendorId`,
`productId`, `guid` / `serial`) become typed; `batteryLevel` and `charging` move
to `DeviceState`; `rawHidAvailable` becomes `Capabilities::RAW_HID`.

### 4.2 Reports

```rust
#[repr(u8)]
pub enum GamepadButton { A, B, X, Y, Select, Guide, Start, LeftStick, RightStick,
    L, R, DpadUp, DpadDown, DpadLeft, DpadRight, Misc1,
    RightPaddle1, LeftPaddle1, RightPaddle2, LeftPaddle2, Touchpad }

#[derive(Clone, Copy, Default, PartialEq)]
pub struct GamepadReport {
    pub buttons: u32,
    pub left_stick: [i16; 2],
    pub right_stick: [i16; 2],
    pub left_trigger: u16,
    pub right_trigger: u16,
}

#[derive(Clone, Copy, Default, PartialEq)]
pub struct SensorReport {
    pub accelerometer: [f32; 3],
    pub gyroscope: [f32; 3],
    pub orientation: [f32; 4],
    pub has_orientation: bool,
    pub sampled_at_ns: u64,
}

#[derive(Clone, Copy, Default, PartialEq)]
pub struct BatteryState { pub level: Option<u8>, pub charging: bool }

#[derive(Clone, Copy, Default, PartialEq)]
pub struct DeviceState {
    pub gamepad: GamepadReport,
    pub sensor: SensorReport,
    pub pointer: PointerReport,
    pub battery: BatteryState,
}
```

`PointerReport` is defined in §15.8. Note the split it implies: `DeviceState`
carries the **resolved** pointer (wire form — projected, device-absolute, or
relative depending on the port's `PointerMode`), while the backend publishes the
**native** `PointerSample` (§15.3) on a separate path. The hub retains the
latest sample per device and exposes it, so that a viewer-side projector can
read it in `AbsoluteScreen` mode and nothing reads it in the other modes:

```rust
impl BackendHandle {
    pub fn publish_pointer(&self, device: &DeviceId, sample: &PointerSample);
}

impl InputHub {
    pub fn pointer_sample(&self, index: PlayerIndex) -> Option<PointerSample>;
    pub fn set_pointer_override(&self, index: PlayerIndex, pointer: Option<PointerReport>);
}
```

`DeviceState` is `Copy` and layout-stable so it can be `IntoBytes` / `FromBytes`
on the wire without a second mirror struct — this is what kills the
`GamepadState` → `SlotState` → `GamepadUpdate` → `WireMessage` chain.

`GamepadButton` ordering stays pinned to SDL wire order; the existing
`wire_order_matches_the_pad` check moves to `polymer-input-layout`.
`GamepadButton::binding()` (the `BindingKey` mapping) **moves out** — the hub has
no `consolespec` dependency. It becomes `polymer_input_layout::sdl::binding_of`.

### 4.3 Backend (HAL) trait

```rust
pub trait InputBackend: Send + Sync + 'static {
    fn id(&self) -> BackendId;
    fn start(&self, handle: BackendHandle) -> Result<(), BackendError>;
    fn shutdown(&self);

    fn apply_feedback(&self, device: &DeviceId, feedback: &Feedback)
        -> Result<(), BackendError>;

    fn set_calibration(&self, device: &DeviceId, calibration: &Calibration)
        -> Result<(), BackendError>;

    fn open_hd_rumble(&self, _device: &DeviceId, _format: HdRumbleFormat)
        -> Result<Box<dyn HdRumbleSink>, BackendError> { Err(BackendError::Unsupported) }

    fn send_raw_hid(&self, _device: &DeviceId, _report: &[u8]) -> Result<(), BackendError> {
        Err(BackendError::Unsupported)
    }
    fn set_raw_hid_capture(&self, _device: &DeviceId, _enabled: bool)
        -> Result<(), BackendError> { Err(BackendError::Unsupported) }
    fn raw_hid_filter(&self, _device: &DeviceId) -> Option<Arc<dyn RawHidFilter>> { None }

    fn recenter(&self, _device: &DeviceId) -> Result<(), BackendError> {
        Err(BackendError::Unsupported)
    }
}
```

Backends push through the handle the hub gives them. The handle is the only way
in, so the hub can sequence and fan out without the backend knowing about
indices:

```rust
#[derive(Clone)]
pub struct BackendHandle { /* Arc<HubInner>, BackendId */ }

impl BackendHandle {
    pub fn connected(&self, descriptor: DeviceDescriptor);
    pub fn disconnected(&self, device: &DeviceId);
    pub fn publish(&self, device: &DeviceId, state: &DeviceState);
    pub fn publish_raw_hid(&self, device: &DeviceId, report: &[u8]);
    pub fn unsupported(&self, device: &DeviceId, reason: &str);
}
```

`publish` takes `&DeviceState`; the hub copies once into its registry slot and
listeners receive a borrow of that slot. Nothing on this path allocates.

### 4.4 Listener traits

```rust
pub enum InputEvent<'a> {
    Connected { index: Option<PlayerIndex>, device: &'a DeviceDescriptor },
    Disconnected { index: Option<PlayerIndex>, device: &'a DeviceId },
    Assigned { device: &'a DeviceId, from: Option<PlayerIndex>, to: Option<PlayerIndex> },
    State { index: PlayerIndex, sequence: u32, device: &'a DeviceId, state: &'a DeviceState },
    RawHid { index: PlayerIndex, device: &'a DeviceId, report: &'a [u8] },
}

pub trait InputListener: Send + Sync + 'static {
    fn on_input(&self, event: InputEvent<'_>);
    fn wants_raw_hid(&self, _device: &DeviceDescriptor) -> bool { false }
}
```

Keyboard and mouse are a separate plane, per the design:

```rust
pub enum HostInputEvent {
    Key { code: KeyCode, scan_code: u16, virtual_key: u16,
          pressed: bool, repeat: bool, extended: bool,
          target_window: i64, timestamp_ms: u32, sequence: u32 },
    Pointer(PointerInput),
    MouseButton { button: u8, pressed: bool, timestamp_ms: u32 },
    MouseMotion { dx: f64, dy: f64 },
    Wheel { horizontal: i32, vertical: i32 },
    FocusLost,
}

pub trait HostInputListener: Send + Sync + 'static {
    fn on_host_input(&self, event: &HostInputEvent);
}
```

`KeyCode` is the DOM `code` string interned as `&'static str` — the vocabulary
`DEFAULT.kb.toml` and the shell already use. `scan_code` / `virtual_key` travel
alongside so the Win32 and DirectInput hooks do not re-derive them.

Handles returned on registration:

```rust
pub struct ListenerHandle { /* Arc<HubInner>, ListenerId */ }

impl ListenerHandle {
    pub fn devices(&self) -> Vec<(Option<PlayerIndex>, DeviceDescriptor)>;
    pub fn state(&self, index: PlayerIndex) -> Option<DeviceState>;
    pub fn raw_controls(&self, index: PlayerIndex) -> Option<RawControls>;
    pub fn feedback(&self, index: PlayerIndex, feedback: Feedback) -> Result<(), HubError>;
    pub fn open_hd_rumble(&self, index: PlayerIndex, format: HdRumbleFormat)
        -> Result<HdRumbleStream, HubError>;
    pub fn send_raw_hid(&self, index: PlayerIndex, report: &[u8]) -> Result<(), HubError>;
    pub fn set_raw_hid_capture(&self, index: PlayerIndex, enabled: bool) -> Result<(), HubError>;

    pub fn calibration(&self, device: &DeviceId) -> Calibration;
    pub fn calibrate(&self, device: &DeviceId, calibration: &Calibration) -> Result<(), HubError>;
    pub fn reset_calibration(&self, device: &DeviceId) -> Result<(), HubError>;
    pub fn begin_calibration(&self, device: &DeviceId, kind: CalibrationKind)
        -> Result<CalibrationCapture, HubError>;

    pub fn detach(self);
}
```

Registration replays: `add_listener` immediately delivers `Connected` for every
known device and one `State` per assigned index, so a listener attaching
mid-session is not blind until the next report. This replaces both
`RoutedGamepadSink::attach`'s replay and `InputPublisher::accept`'s catch-up
loop with one implementation.

### 4.5 Feedback

```rust
pub enum Feedback {
    Rumble { low_frequency: u16, high_frequency: u16, duration_ms: u32 },
    HdRumble(HdRumbleFrame),
    PlayerLed { index: u8 },
    HomeLed { brightness: u8 },
    Recenter,
    Calibrate(Calibration),
}

pub struct HdRumbleFrame { pub left: HdRumbleChannel, pub right: HdRumbleChannel }

pub struct HdRumbleChannel {
    pub low_frequency_hz: f32,
    pub low_amplitude: f32,
    pub high_frequency_hz: f32,
    pub high_amplitude: f32,
}
```

HD rumble **audio streams** get a streaming entry point rather than an event —
`ListenerHandle::open_hd_rumble` → `HdRumbleStream::submit(&[f32])` →
`InputBackend::open_hd_rumble` → `Box<dyn HdRumbleSink>`.

**Neither backend implements it in this refactor.** `polymer-bluetooth` exposes
only `set_rumble` and `set_player_index`; SDL3 exposes only
`SDL_RumbleGamepad`. Define the trait, return `BackendError::Unsupported`, gate
on `Capabilities::HD_RUMBLE`, and land Switch / Switch 2 HD rumble separately.

**Feedback arbitration is a real decision, not a detail.** Two listeners (the
Deno overlay and the IPC hook) can drive index 0 simultaneously.

- `Rumble` / `HdRumble`: single owner. The hub tracks a feedback owner set by
  `InputHub::set_feedback_priority(Option<ListenerId>)`; other listeners' rumble
  is dropped, not merged. This matches today, where only the injected target
  sends `GamepadFeedback`.
- `PlayerLed`: hub-owned, never listener-owned. The hub drives it from the index
  it assigned, so the LED always agrees with the index. Listener requests are
  rejected with `HubError::HubOwned`.
- `Recenter`: any listener, forwarded unconditionally.
- `Calibrate`: any listener, **latched and replayed** — see below and §16.

Arbitration is per variant, not per channel. Make that explicit rather than
implicit, because the variants differ in both who may send and whether the hub
retains them:

```rust
pub enum FeedbackClass {
    Actuation,
    Managed,
    Latched,
    Transient,
}

impl Feedback {
    pub const fn class(&self) -> FeedbackClass {
        match self {
            Self::Rumble { .. } | Self::HdRumble(_) => FeedbackClass::Actuation,
            Self::PlayerLed { .. } => FeedbackClass::Managed,
            Self::Calibrate(_) | Self::HomeLed { .. } => FeedbackClass::Latched,
            Self::Recenter => FeedbackClass::Transient,
        }
    }
}
```

| Class | Who may send | Retained? |
| --- | --- | --- |
| `Actuation` | feedback-priority owner only | no |
| `Managed` | hub only | yes, derived from the assigned index |
| `Latched` | any listener | yes, per device |
| `Transient` | any listener | no |

**`Latched` feedback is retained per `DeviceId` and re-applied on `Connected`.**
This is not optional. `ControllerSession::synchronize` re-runs on every
reconnect *and* on `set_external_ownership(false)` — leaving raw HID
passthrough — and would otherwise restore the driver's compiled-in defaults,
silently discarding a user's configuration. The hub owns the latch so no backend
has to.

### 4.6 The hub

```rust
pub struct InputHub { inner: Arc<HubInner> }

pub struct AssignmentPolicy {
    pub max_players: u8,
    pub auto_assign: bool,
    pub auto_compose: bool,
    pub retain_disconnected: bool,
}

impl InputHub {
    pub fn new(policy: AssignmentPolicy) -> Self;

    pub fn register_backend(&self, backend: Arc<dyn InputBackend>) -> Result<(), HubError>;
    pub fn add_listener(&self, listener: Arc<dyn InputListener>) -> ListenerHandle;
    pub fn add_host_listener(&self, listener: Arc<dyn HostInputListener>) -> HostListenerHandle;

    pub fn publish_host(&self, event: HostInputEvent);

    pub fn assign(&self, device: &DeviceId, index: Option<PlayerIndex>) -> Result<(), HubError>;
    pub fn assignments(&self) -> Vec<(PlayerIndex, DeviceId)>;
    pub fn devices(&self) -> Vec<(Option<PlayerIndex>, DeviceDescriptor)>;
    pub fn snapshot(&self) -> Vec<(Option<PlayerIndex>, DeviceDescriptor, DeviceState)>;

    pub fn compose(&self, members: &[DeviceId], mode: CompositionMode)
        -> Result<DeviceId, HubError>;
    pub fn decompose(&self, composite: &DeviceId) -> Result<(), HubError>;

    pub fn set_feedback_priority(&self, listener: Option<ListenerId>);
    pub fn set_max_players(&self, max: u8);
}
```

**Composition stays in the hub**, not the HAL: pairing two Joy-Cons is an
identity decision, not a decode decision. The backend reports
`DeviceRole::LeftHalf` / `RightHalf`; `compose` mints a synthetic
`DeviceId::new(BackendId::VIRTUAL, "pair/<a>+<b>")` whose descriptor unions the
members' capabilities and whose state merges buttons and triggers by OR / max
and sticks by role — today's `RouterState::compose`, moved to
`hub/assignment.rs` and stripped of remapping. Feedback and raw HID addressed to
a composite fan out to its members.

**Index stability rules**, implicit today, now explicit:

- An index survives disconnect when `retain_disconnected` is set; the hub emits
  `Disconnected` but keeps the device→index binding.
- Reconnect of a retained `DeviceId` restores its index and emits `Connected`
  with that index. No `Assigned` event.
- `auto_assign` fills the lowest free index below `max_players` on first connect.
- Explicit `assign` always wins and can displace an auto-assignment, emitting
  `Assigned` for both devices.
- `assign(device, None)` unbinds without disconnecting.

---

## 5. `polymer-input-layout` — the mapping helper

Hook-safe. Contains, moved verbatim except for comment removal:

```
crates/input/polymer-input-layout/src/
  lib.rs
  layout/mod.rs     InputLayout, InputElement, Cluster, SdlOutputLayout,
                    ProjectionMode, RemapDatabase, RemapProfile, EMBEDDED_* consts
  sdl/mod.rs        Wire, SdlSensor, SdlButton, SdlAxis, wire(), has(),
                    decompose(), binding_of(GamepadButton)
  keyboard/mod.rs   KeyboardMapping (TOML documents)
  mapping/mod.rs    NEW: the applier
```

The asset directories `SDL_GameControllerDB/` and `keyboard/` move with it.

The new part is the applier the hooks run:

```rust
pub struct Mapping {
    buttons: [Option<Source>; 21],
    axes: [Option<AxisSource>; 8],
    keys: Vec<(KeyCode, Target)>,
}

pub enum Source { Button(u8), Key(KeyCode), MouseButton(u8) }

pub struct AxisSource { pub source: AxisInput, pub invert: bool,
                        pub scale: f32, pub dead_zone: f32 }

pub enum AxisInput { Axis(u8), KeyPair(KeyCode, KeyCode),
                     MouseDelta(PointerAxis), MousePosition(PointerAxis) }

impl Mapping {
    pub fn compile(layout: &InputLayout, mode: ProjectionMode,
                   bindings: &[(BindingKey, Option<BindingKey>)])
        -> Result<Self, MappingError>;

    pub fn identity(layout: &InputLayout, mode: ProjectionMode) -> Self;

    pub fn apply(&self, source: &GamepadReport, host: &HostSample) -> GamepadReport;

    pub fn consumes_key(&self, code: KeyCode) -> bool;
}

pub struct HostSample {
    pub keys: KeySet,
    pub mouse_buttons: u8,
    pub pointer: Option<[i16; 2]>,
    pub delta: [f32; 2],
}
```

This is the **only** place mapping happens after the refactor. It runs in three
places, all reading the same compiled form:

1. Inside `polymer-hook-input` (target side, per virtual pad).
2. Inside the Deno overlay listener (host side, for overlay navigation).
3. Inside the GUI, for the binding-capture UI preview and `consumes_key`.

`ProjectionMode::{Name, Position, Custom}` and the default-binding derivation
(`labelled_face_target_for`, `positional_face_target`, `apply_default_axes`,
`profile_matches`, the `gamecontrollerdb` remap database) move here from
`provider/sdl3/mod.rs` with behaviour unchanged.

Dependency direction: `polymer-input-layout` depends on `polymer-input`,
never the reverse. See §14.7.

---

## 6. `polymer-input-sdl3` — SDL HAL backend

Survives from `provider/sdl3/mod.rs`: the SDL thread, event pump, device
enumeration and identity (`source_registry.rs` moves verbatim), gamepad open /
close, sensor enable, touchpad→pointer, and the poll loop — roughly 900 lines.

It publishes a `DeviceDescriptor` per open SDL gamepad and a raw `DeviceState`
per poll: **unmapped**, in SDL positional order. `SdlButton::South` becomes
`GamepadButton::A` positionally and nothing else happens to it.

```rust
pub struct Sdl3Backend { /* thread, stop flag, command channel */ }

pub struct Sdl3Config {
    pub gamecontroller_database: Option<PathBuf>,
    pub poll_interval: Duration,
}

impl Sdl3Backend {
    pub fn new(config: Sdl3Config) -> Result<Self, BackendError>;
}

impl InputBackend for Sdl3Backend { /* … */ }
```

Deleted from this crate: `SlotConfig`, `ProviderConfig::slots`,
`reconfigure_slots`, `set_binding`, `set_key_binding`, `set_mouse_axis_binding`,
`clear_binding`, `reset_bindings`, `set_key`, `clear_keys`, `is_key_mapped`,
`set_pointer`, `add_mouse_delta`, `PhysicalGamepadState`, `PhysicalDeviceState`,
`set_enabled`.

`set_enabled` is the subtle one: it was a *routing* concept (neutralize mapped
output while keeping diagnostics live). It becomes
`InputHub::set_feedback_priority` plus the listener choosing not to forward —
see §14.10. The backend never gates.

Diagnostics the GUI reads from `PhysicalDeviceState` (raw per-button and
per-axis values for binding capture) come instead from
`ListenerHandle::raw_controls(index)`, backed by the `DeviceState` the hub
already holds. "Which controls does the active mapping consume" highlighting
moves to the GUI, which now owns the `Mapping`.

`build.rs` moves here unchanged.

---

## 7. `polymer-input-bluetooth` — Bluetooth HAL backend

Role unchanged; surface changes.

```rust
pub struct BluetoothBackend { /* manager, decoders, task */ }

impl BluetoothBackend {
    pub async fn start(config: ManagerConfig) -> Result<Self, BackendError>;
    pub fn manager(&self) -> BluetoothManager;
    pub fn discovery(&self) -> broadcast::Receiver<DiscoveryEvent>;
}

impl InputBackend for BluetoothBackend { /* … */ }
```

- `BluetoothControllerState` is **deleted**. Its fields split into
  `DeviceDescriptor` (name, profile, vendor, product, role, capabilities) and
  `DeviceState` (gamepad, sensor, pointer, battery). `report_count` was
  diagnostics only; drop it or move it behind `polymer-statistics`.
- The `BluetoothProviderEvent` broadcast is **deleted for input**. The per-report
  `Box<BluetoothControllerState>` clone goes away entirely. A separate low-rate
  `DiscoveryEvent` broadcast keeps the pairing / scan UI working —
  `AdapterReady`, `AdapterUnavailable`, `ScanState`, `Discovered`, `Connecting`,
  `FirmwareLoaded`, `Error`. Input reports do not travel on it.
- `decoder::DecodedInput` folds what the hub can carry into `SensorReport`
  (including the orientation quaternion, which is currently computed in
  `MotionFrame` and then **thrown away at the router boundary**). The rest of
  `MotionFrame` stays internal to the decoder.
- `role_for_profile` stays; it feeds `DeviceDescriptor::role`.
- `ControllerSession` gains `set_calibration`, and `WiimoteSession` retains it so
  `synchronize` reprograms the IR camera from it instead of the compiled-in
  sensitivity blocks and `CAMERA_BASIC_MODE` (§15.6). This is on the same trait
  as `set_rumble` and `set_player_led` because it is the same kind of traffic.
- `decoder::normalize_stick`'s `(raw - 2048) * 32767 / 2047` and the decoders'
  hardcoded `/ 4096.0 * GRAVITY` and `*_RAW_PER_RADIAN_PER_SECOND` constants
  become the *stated per-profile factory value*, held inside this crate and
  never exposed (§16.4). The backend composes it with the user layer it is
  given and publishes already-calibrated reports. Real factory reads — Switch
  SPI, Wii Remote EEPROM `0x0016` — replace the defaults with no API change and
  are deferred (§16.9).
- `raw/mod.rs` splits: the **transport** (named pipe server, connect / retry,
  external-ownership handshake, 5 s grace period) moves to
  `polymer-input-ipc`; the **device-specific filtering** stays here behind a
  HAL hook:

```rust
pub trait RawHidFilter: Send + Sync {
    fn filter_input(&self, report: &mut [u8], capture: bool);
    fn filter_output(&self, report: &[u8], capture: bool) -> Cow<'_, [u8]>;
    fn buttons_clear(&self, report: &[u8]) -> bool;
}
```

`ReportMetadata::filter_for_dolphin`, the capture gate, and the
speaker-report→rumble rewrite become the Wii Remote implementation of it. The
*policy* — is capture requested? — comes from the hub via `set_raw_hid_capture`,
driven by UI priority. That is what `BluetoothHandle::set_raw_capture` does
today, but it no longer routes through the GUI's Bluetooth handle.

---

## 8. `polymer-input-ipc` — the IPC listener

Absorbs `polymer-runtime/src/input.rs`, `polymer-ipc/src/input.rs`, the
transport half of `polymer-input-bluetooth/src/raw/mod.rs`, and the host half
of `polymer-input-dinput`.

```
crates/input/polymer-input-ipc/src/
  lib.rs
  publisher/mod.rs   shared-memory gamepad + host-input publication
  control/mod.rs     the target control socket, feedback fan-in
  topology/mod.rs    layout/remap revision publication (§9)
  hid/mod.rs         per-device duplex raw-HID pipes
```

```rust
pub struct InputIpcListener { /* … */ }

impl InputIpcListener {
    pub fn bind(session: &str) -> io::Result<Self>;
    pub fn publish_topology(&self, topology: &InputTopology) -> io::Result<()>;
    pub fn open_raw_hid(&self, index: PlayerIndex, spec: RawHidSpec) -> io::Result<RawHidChannel>;
    pub fn shutdown(&self);
}

impl InputListener for InputIpcListener { /* … */ }
impl HostInputListener for InputIpcListener { /* … */ }
```

### 8.1 Wire changes in `polymer-ipc`

Bump `PROTOCOL_VERSION` 16 → 17.

Replace `Message::{GamepadUpdate, VirtualGamepadConnection, DirectInputData}`
with:

| Message | Direction | Carried on |
| --- | --- | --- |
| `DeviceConnected(WireDeviceConnected)` | host → target | shared ring |
| `DeviceDisconnected { index }` | host → target | shared ring |
| `DeviceState(WireDeviceState)` | host → target | shared ring |
| `HostKey(WireHostKey)` | host → target | shared ring |
| `HostPointer(WirePointerInput)` | host → target | shared ring |
| `HostWheel(WireHostWheel)` | host → target | shared ring |
| `GamepadFeedback(WireFeedback)` | target → host | control socket |
| `Blob { kind: Topology, len }` + payload | host → target | control socket, variable length |

`WireDeviceState` is `DeviceState` with `zerocopy` little-endian scalars — one
struct, no `SlotState` mirror, no `GamepadUpdate` / `GamepadState` triple.

**Fix the per-message allocation.** `encode_input` currently returns a `Vec<u8>`
sized by `PAYLOAD_SIZE`, which is dominated by `WireStreamReady`'s image-handle
array. Give the input channel its own compact `WireInputMessage` sized by the
input messages only, and have `shared::Publisher::publish` write from a reusable
per-publisher scratch buffer instead of a fresh `Vec`. Expect the input frame to
drop from ~1 kB to ~72 bytes and the allocation to disappear.

`polymer-ipc` loses its `polymer-input` dependency: it takes
`PointerInput` / `RawKeyInput` from `polymer-protocol` and defines the input
wire structs itself.

### 8.2 Raw HID channel

The `bluetoothapi-win32` hook needs a real OS `HANDLE` supporting overlapped
`ReadFile` / `WriteFile` with message framing, because it redirects the target's
`CreateFileW` on a synthetic HID device path
(`polymer-hook-bluetoothapi-win32/src/runtime/mod.rs::create_file`). **The
shared-memory ring cannot serve this.** Raw HID therefore stays a per-device
duplex named pipe, with its lifecycle moved under the IPC listener:

```rust
pub struct RawHidSpec {
    pub pipe_name: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub report_size: usize,
}
```

- Host→target reports arrive as `InputEvent::RawHid` and are written to the pipe
  after `RawHidFilter::filter_input`.
- Target→host writes are read from the pipe, passed through `filter_output`, and
  returned via `ListenerHandle::send_raw_hid`.
- `report_size` replaces the hard-coded `DOLPHIN_INPUT_REPORT_SIZE = 22`.
- The 5-second grace period on pipe closure (Dolphin deliberately closes every
  Real Wiimote during an atomic topology rebind) **must be preserved**. It is
  load-bearing and non-obvious.

The pipe name still reaches the hook, but through the topology control message
rather than `POLYMER_BLUETOOTH_HID_PIPES`. The hook builds its
`SyntheticWiimote` table from that message and can rebuild on revision change —
which the env-var version could not do.

---

## 9. Topology control channel

The runtime tells hooks what layout each player index drives, which emulator
port it maps to, how to remap, and whether remapping applies at all.

```rust
#[derive(Serialize, Deserialize)]
pub struct InputTopology {
    pub revision: u32,
    pub machine: Option<String>,
    pub ports: Vec<PortBinding>,
    pub raw_hid: Vec<RawHidBinding>,
}

#[derive(Serialize, Deserialize)]
pub struct PortBinding {
    pub player: u8,
    pub group: String,
    pub port: u8,
    pub layout: String,
    pub mode: RemapMode,
    pub bindings: Vec<(String, Option<String>)>,
    pub pointer: PointerBinding,
}

pub enum RemapMode { Position, Name, Custom, Passthrough }

pub struct PointerBinding { pub source: PointerSource, pub mode: PointerMode }

pub enum PointerSource { None, DeviceNative, HostMouse, DeviceThenHostMouse }

pub enum PointerMode { None, Relative, AbsoluteDevice, AbsoluteScreen }

pub struct RawHidBinding {
    pub player: u8, pub port: u8,
    pub vendor_id: u16, pub product_id: u16,
    pub pipe: String,
}
```

`RemapMode::Passthrough` is what `bluetooth-api-win32` gets: "do not remap; the
HID stream is authoritative". A hook that cannot remap ignores `bindings` and
says so at handshake.

Transport: `WireMessage` is fixed-size, so add one framed message —
`Message::Blob { kind: BlobKind, len: u32 }` followed by `len` bytes, with
`LocalSocket::send_blob(kind, bytes)`, encoded with `bincode` (§14.13). It is a
control-plane message published on change, not per frame, but the decoder runs
inside every injected hook DLL, so a binary format is worth the small loss in
debuggability over JSON.

Consequences:

- `EmulatorLaunchConfig::gamepad_layouts` is deleted.
- `polymer-hook::config::{LAYOUT, LAYOUTS, SDL_GAMEPADS, BLUETOOTH_HID_PIPES}`
  and accessors are deleted.
- `polymer-runtime/src/launch.rs::hook_environment` loses the layout, gamepad
  count, and Bluetooth pipe blocks (about 30 lines).
- `polymer-hook-sdl3/src/install.rs::create_table_state` no longer reads env;
  it constructs an empty pad set and rebuilds on the first topology.
- **SDL virtual joysticks must be detached and reattached on a topology change**
  — the button / axis / sensor mask changes with the layout. `SdlBackend` gains
  `fn rebind(&mut self, layouts: &[SdlOutputLayout]) -> Result<(), BackendError>`,
  and `Runtime::flush` must not publish across a rebind boundary.

---

## 10. Target side: `polymer-hook-input` and friends

```
crates/hooks/polymer-hook-input/src/
  lib.rs
  subscriber/mod.rs   IPC subscriber thread, reconnect, feedback socket
  state/mod.rs        per-index SeqLock<DeviceState>, connection bits
  host/mod.rs         keyboard/mouse queues + KeySet, HostSample construction
  topology/mod.rs     current InputTopology + compiled Mapping per index
```

```rust
pub struct HookInput { /* … */ }

impl HookInput {
    pub fn start(session: &str) -> Arc<Self>;
    pub fn topology(&self) -> TopologyGuard<'_>;
    pub fn mapped(&self, player: u8) -> Option<GamepadReport>;
    pub fn raw(&self, player: u8) -> Option<DeviceState>;
    pub fn host(&self) -> &HostState;
    pub fn send_feedback(&self, player: u8, feedback: Feedback) -> bool;
}
```

`mapped()` applies the compiled `Mapping` against the raw `DeviceState` plus the
current `HostSample`. This is the single place keyboard-as-gamepad projection
now lives.

The per-API hooks become thin:

- `polymer-hook-input-sdl` — `SdlBackend` trait, virtual joystick attach /
  detach / rebind, publishes `mapped()` into SDL. Retains the sequence-newer
  wrap check and the coalescing flush.
- `polymer-hook-input-win32` — consumes `HostState` pointer and key events and
  posts window messages. Essentially unchanged; it already subscribes to this
  channel.
- `polymer-hook-input-dinput` — consumes `HostState` and **now owns**
  `RawKeyInput::direct_input_data()` (moved from `polymer-input/src/lib.rs`),
  synthesising `DIDEVICEOBJECTDATA` records target-side instead of the viewer
  pre-encoding them. This is the concrete win from "hooks synthesize the correct
  expected inputs for virtual devices".

---

## 11. `polymer-controllers` — host-side emulator controller layer

Everything consolespec-aware that the hub sheds. Moved, not rewritten, except
where it must talk to the new hub.

| Source | Destination |
| --- | --- |
| `polymer-input/src/hub.rs` | `polymer-controllers/src/configuration/mod.rs`; type renamed `InputHub` → `ControllerConfiguration` (the name `InputHub` now belongs to the router) |
| `polymer-input/src/topology/mod.rs` | `polymer-controllers/src/topology/mod.rs` |
| `polymer-runtime/src/controllers/**` | `polymer-controllers/src/manager/**` |
| `polymer-runtime/src/controllers/mapping/mod.rs` | `polymer-controllers/src/profile/mod.rs` |

Changes while moving:

- `BindingOverride::sdl_binding()` is deleted. `ControllerBindings`
  (`BindingKey → Option<BindingKey>`) becomes the **single** binding
  representation, and `PortConfiguration::overrides` uses it. This collapses
  representations (2), (3), (4) from §2.2 into one.
- `ControllerManager::routes_for` and `resolve_configuration` are deleted.
  `sync_physical_routes` becomes a loop of `InputHub::assign` plus one
  `InputIpcListener::publish_topology`.
- `observe_joycon_pair`'s `session.is_none()` early return is deleted along with
  the rest of its logic; automatic composition moves to `hub/assignment.rs` under
  `auto_compose` (§14.6), so a bonded pair re-forms on background reconnect. The
  manager keeps only the gesture path, which genuinely needs a session.
- `ControllerManager::observe` no longer takes `&[PhysicalDeviceState]` and
  `&[BluetoothControllerState]`. It takes
  `&[(Option<PlayerIndex>, DeviceDescriptor, DeviceState)]` from
  `InputHub::snapshot()`. The join-edge / sideways-Wiimote / Joy-Con-pair
  detection transfers directly and stops being backend-specific: the
  `switch_controller` special case reads `descriptor.profile`, not
  `BluetoothControllerState`.
- Auto-assignment (`assign_if_open`, `next_open`, `BASE_PLAYERS`,
  `SESSION_PLAYERS`) moves to `AssignmentPolicy` in the hub. The manager keeps
  only the session semantics (press-to-join gating), calling `hub.assign` when
  an edge fires.
- `PortConfiguration::slot` keeps its meaning: it is the translation from
  emulator port to hub player index, and it is exactly why the hub must not know
  about ports. Restate the port-index-skew note where it now lives.

---

## 12. Deno / JS surface

`Polymer.input` is only the input event/state surface. It does not enumerate
or command physical devices.

```js
class InputClient extends EventTarget {
  get(index)     // { buttons, leftStick, rightStick, leftTrigger,
                 //   rightTrigger, accelerometer, gyroscope, orientation,
                 //   pointer, battery }
  // event: "input"
}
```

`Polymer.controllers` owns the physical-device surface: enumeration,
assignment, discovery and connection sessions, feedback, calibration, and
mapping profiles.

```js
class ControllersClient extends EventTarget {
  get devices()  // [{ index, id, name, profile, vendorId, productId,
                 //    role, capabilities, connected, battery }]
  get players()
  get connected()
  rumble(deviceId, { lowFrequency, highFrequency, durationMs })
  recenter(deviceId)
  calibrate(deviceId, calibration)
  getCalibration(deviceId)
  resetCalibration(deviceId)
  beginCalibration(deviceId, kind)
  configurePointer(deviceId, { sensorBar, sensitivity, mode })  // calibrate() wrapper
  // events: "controllerconnected", "controllerdisconnected", "playerchange",
  //         "devicechange", "statechange"
}
```

These send `Feedback::Calibrate` (§16) — device command traffic, not host-side
settings, even where the backend realizes them in software. The shell persists
the user layer in localStorage keyed by device id and pushes it on
`deviceconnected` and on change; the hub's `Latched` retention covers reconnects
the shell does not observe. There is no Rust persistence for it.

Backed by a `DenoInputListener` in `polymer-runtime` implementing
`InputListener`. It must **not** serialize on every report: expose a `get(index)`
op reading the hub's current state on demand, and fire `input` only on change,
coalesced to the JS event-loop turn. The 60 Hz `serde_json::Value` rebuild in
`bins/polymer-gui/src/input/publication/mod.rs` is exactly the pattern to
avoid.

`Polymer.controllers` (`07_controllers.js`) is backed by
`polymer-controllers` for manager state and by device-addressed hub commands
for feedback and calibration. `device.metadata` becomes typed fields — update
`js/polymer.ts` `ControllerDevice`, `plugins/plugin-api.ts`, and
`plugins/dolphin/dolphin.ts::rawWiimote`, which reads
`device.metadata.profileId`, `.productId`, and `.rawHidAvailable`.

`InputProvider` becomes a raw-HID request rather than a pipe description — the
plugin should not know about pipe names:

```ts
export type InputProvider = Readonly<{
    type: "rawHid";
    sourceId: string;
    port: number;
    productId: number;
}>;
```

`polymer-pluginhost::InputProvider::BluetoothHidPipe` → `RawHid`, and
`polymer-runtime::BluetoothHidPipeConfig` is deleted in favour of
`RawHidBinding` in the topology message.

---

## 13. Migration order

Each phase must leave the workspace building. Do not merge phases.

1. **Extract wire types.** Move `PointerInput`, `PointerPhase`, `PointerSource`,
   `DirectInputData`, `DirectInputDevice`, `RawKeyInput` to
   `polymer-protocol`. Move `mouse::layout` to `polymer-renderer`. Update
   the call sites in §3.4. No behaviour change.
2. **Create `polymer-input-layout`.** Move `layout.rs`, `sdl.rs`, the keyboard
   documents, and the `SDL_GameControllerDB` / `keyboard` asset directories.
   Point `polymer-input` and the SDL hooks at it. No behaviour change.
3. **Create `polymer-controllers`.** Move `hub.rs`, `topology/`, and
   `polymer-runtime::controllers`. Collapse `BindingOverride` /
   `InputBinding` / `ControllerBindings` into one type. Delete
   `ResolvedInputConfiguration`.
4. **Rewrite `polymer-input` as the hub.** New `device` / `report` /
   `feedback` / `backend` / `listener` / `hub` modules, the merged
   `Capabilities` bitfield, and `AssignmentPolicy` including `auto_compose` and
   the session-free composition rule of §14.6. Delete `router`, `source`,
   `gamepad`, `provider`, `hub`, `topology`, `sdl`, `layout`, `mouse`,
   `build.rs`.
5. **Create `polymer-input-sdl3`.** Move `provider/sdl3` + `build.rs`, strip
   mapping, implement `InputBackend`.
6. **Convert `polymer-input-bluetooth`.** Implement `InputBackend`, delete
   `BluetoothControllerState` and the input broadcast, add `RawHidFilter`.
7. **Create `polymer-input-ipc`.** Bump `PROTOCOL_VERSION` to 17, replace the
   input wire messages, fix the per-message allocation, add the blob frame and
   the raw-HID channel. Delete `polymer-runtime/src/input.rs` and
   `polymer-ipc/src/input.rs`.
8. **Move and rewrite the target side.** Create `polymer-hook-input`, relocate
   `polymer-input-{sdl,win32,dinput}` into `crates/hooks/`, delete the host
   halves, wire the topology channel, delete the env-var config.
9. **Rewire the GUI and runtime.** `bins/polymer-gui/src/input.rs` shrinks to
   priority / focus / pointer policy plus hub registration; delete
   `reconcile.rs`, `RoutedGamepadSink`, `RoutedInputSender`; rewrite
   `publication/mod.rs` against `InputHub::snapshot()`.
10. **Pointer modality (§15.1–15.4, 15.8–15.9).** Convert the four decoders to
    `PointerSample`, delete the decoder-side accumulate-and-clamp, add
    `PointerCapabilities`, add `PointerBinding` to the topology channel, and
    implement `Relative` and `AbsoluteDevice`. **Dolphin works at the end of
    this step** and never enters the projector.
11. **Pointer projection (§15.5–15.7, 15.11).** Add
    `polymer-renderer/src/pointer/`, refit `PointerRoutingState` onto
    `PointerProjector`, add `PointerTarget::Surface` and the override path.
    Identity calibration plus the sensor bar offset only. This step is what
    azahar and melonDS need; nothing else depends on it.
12. **Calibration (§16).** `Calibration` as a write-only HAL surface,
    `correct` / `shape`, `CalibrationFields` on the descriptor, the `Latched`
    replay, and the composing capture flow. Per-profile defaults held inside
    each backend; no device reads yet. Folds `PointerSetup` in as one field.
13. **Deno surface.** Real event-only `Polymer.input` listener; device-owned
    `Polymer.controllers` with typed `ControllerDevice`, pointer capabilities,
    and `calibrate` / `getCalibration` / `beginCalibration` over localStorage;
    `InputProvider::RawHid`. Update `plugins/dolphin/dolphin.ts` and
    `plugins/plugin-api.ts`.

Full-workspace rebuild required after phase 7. Kill leftover target processes
and `polymer-viewer.exe` before re-running anything.

---

## 14. Gaps and unresolved questions

All of these are now settled — see the decisions table at the top. The sections
below record the reasoning and, where a decision changed the design, what it
changed.

### 14.1 Who knows a key is consumed by the game

Today `Sdl3Provider::is_key_mapped` tells the GUI whether a key belongs to the
game (`InputRouter::route_game_key`). If mapping moves into the hook, the viewer
no longer knows.

**Recommendation:** the GUI keeps its own compiled `Mapping` — it already needs
one for binding capture — and answers `consumes_key` locally from the same
`InputTopology` it publishes. Hook and viewer compile the same document, so they
agree by construction. The alternative is a `KeysConsumed` message back from the
hook, which adds a round trip and a stale window on topology change.

### 14.2 Pointer

Large enough to need its own treatment. See §15.

### 14.3 Feedback arbitration between listeners

§4.5 proposes single-owner priority. The alternative is additive max across
listeners. Single-owner matches today's behaviour (only the injected target
sends feedback) and is easier to reason about during overlay↔game priority
flips.

### 14.4 HD rumble is API-only in this refactor

Neither HAL can produce it. The trait shape is defined so the API does not have
to break again, but `Capabilities::HD_RUMBLE` will be clear on every device until
Switch / Switch 2 encoders land. Flagged so it is not mistaken for a regression.

### 14.5 Raw HID cannot use the shared ring

Settled in §8.2, restated because it constrains the design: the
`bluetoothapi-win32` hook needs a real kernel handle, so raw HID stays a named
pipe. Any future "raw HID for SDL hooks" uses the same channel. The hub's
`RawHid` event is a *fan-out to pipe writers*, not the transport itself.

### 14.6 Composition is not persisted, but must be rebuilt without a session

**Decided:** compositions and player indices reset on app restart. They are not
written to storage. But they must be re-established for *bonded* devices by the
background reconnect path alone — no `ConnectionSession`, no user gesture. A
session is required only for **fresh bonding**.

The transport half already exists. `ControllerRecord` carries `auto_reconnect`,
and `BluetoothManager` reconnects bonded controllers outside any UI flow through
`Command::WakeDiscovered` (BLE advertisement wake) and `accept_inbound`
(controller-initiated BR/EDR, which is how a Wii Remote reconnects on a button
press). What is missing is that assignment and composition are currently gated
on a session.

Today, in `ControllerManager`:

- `observe_system` / `observe_bluetooth` do auto-assign when `session.is_none()`,
  so a lone reconnecting pad does land in a slot. That part is fine.
- `observe_joycon_pair` returns immediately when `session.is_none()`. So a
  bonded Joy-Con pair reconnecting after a restart comes back as **two separate
  players** and stays that way until the user opens a pairing session and
  performs the L+R gesture again.

That is the behaviour to change. Composition acquires two paths:

| Path | Trigger | When |
| --- | --- | --- |
| **Gesture** | L + R held during a `ConnectionSession` | fresh bonding, or explicit re-pairing |
| **Automatic** | both halves connected, bonded, unassigned | background reconnect, no session |

Because composition is not persisted, the automatic path is a *policy* rather
than a memory — the hub does not know these two halves were previously paired.
That creates one ambiguity worth resolving deterministically rather than
heuristically: four Joy-Cons connecting as four individual players would be
auto-paired into two by a naive rule, against the user's intent.

**Rule:** auto-compose only when exactly one unassigned bonded `LeftHalf` and
exactly one unassigned bonded `RightHalf` are present. Any other count leaves
them individual, and explicit composition — gesture or UI — remains available.
Unambiguous cases resolve silently, which is the common one; ambiguous cases
defer to the user rather than guessing.

An explicit `decompose` sticks for the session, so a user who wants two solo
Joy-Cons is not re-paired on the next reconnect within that run.

This lands in `AssignmentPolicy`:

```rust
pub struct AssignmentPolicy {
    pub max_players: u8,
    pub auto_assign: bool,
    pub auto_compose: bool,
    pub retain_disconnected: bool,
}
```

and the session-gated logic moves out of `ControllerManager` into
`hub/assignment.rs` (§11), leaving the manager owning only the press-to-join
gesture semantics that genuinely need a session.

### 14.7 `polymer-input-layout` depends on `polymer-input`

`Mapping::apply` operates on `GamepadReport`, so the hook-safe layout crate
depends on the hub crate. This is acceptable only because `polymer-input` is
dependency-free (`parking_lot` + `tracing`). If the hub ever grows a host-only
dependency, `GamepadReport` must split into a third leaf crate. Keep the hub
dependency-free and enforce it in review.

### 14.8 Index range and `max_players`

Today `BASE_PLAYERS = 4` and `SESSION_PLAYERS = 8`, but the wire allows 256 and
`LatestGamepadMailbox` allocates 256 slots. **Decided:**
`AssignmentPolicy::max_players` defaults to 8, wire indices stay `u8`. The
256-entry mailbox disappears with the mailbox.

### 14.9 Sequence numbers

Three counters exist today (router, `RoutedGamepadSink`, `InputPublisher`
implicit). The hub owns exactly one, per index, monotonic across reconnects, and
the wire carries it. The hook's `sequence_is_newer` wrap check is retained.

### 14.10 What replaces `set_enabled`?

`Sdl3Provider::set_enabled(false)` neutralized mapped output while keeping
diagnostics live during UI priority. In the new shape the backend never gates:
the IPC listener stops forwarding and publishes one neutral `DeviceState` per
assigned index on the priority flip, matching `RoutedGamepadSink::detach` today.
Verify no stuck button survives the overlay↔game transition.

### 14.11 `polymer-bttest` and `polymer-debug-viewer`

Both consume the old surfaces (`bins/polymer-bttest/src/input/mod.rs`;
`bins/polymer-debug-viewer/src/main.rs:527` and its `gamepad_layouts` /
`gamepad_remap_drafts` UI). The debug viewer in particular drives layouts and
remap drafts from its own UI and needs the topology channel. Budget for it; it
is not a trivial update.

### 14.12 Crate names

This refactor adds five crates and relocates three, so the naming is worth one
pass before any of it exists:

| Proposed | Role | Note |
| --- | --- | --- |
| `polymer-input` | hub and router | **keeps its name, changes its meaning entirely** |
| `polymer-input-layout` | hook-safe layouts, projection, mapping | |
| `polymer-input-sdl3` | SDL HAL backend | collides conceptually with today's `polymer-input-sdl`, which is target-side and moves to `crates/hooks/` |
| `polymer-input-ipc` | IPC listener | |
| `polymer-controllers` | emulator ports, topology, profiles | |
| `polymer-hook-input` | shared target-side runtime | |

Two specific hazards:

- **`InputHub` changes referent.** Today it is the machinespec port
  configuration; after this it is the router, and the old type becomes
  `polymer-controllers::ControllerConfiguration`. Every existing mention of
  "the hub" in the codebase and in conversation flips meaning. An alternative is
  naming the router something else — `InputRouter`, `InputBus` — and leaving
  `InputHub` dead. **Recommendation:** take the name; "hub" is what the new
  thing is, and the old one is being renamed anyway.
- **`polymer-input-sdl3` versus `polymer-hook-input-sdl`.** Host-side SDL
  capture and target-side SDL injection, one letter apart. Consider
  `polymer-input-backend-sdl3` if the pair reads as ambiguous.

### 14.13 Topology control-frame encoding

§9 proposes `serde_json` inside a `Message::Blob` frame for `InputTopology`. It
is a control-plane message published on change, not per frame, so JSON is
adequate and debuggable.

The counterargument is consistency: every other Polymer IPC payload is a
`zerocopy` wire struct, the workspace already depends on `rkyv`, and a hook DLL
paying for `serde_json` to read one message is a real code-size cost in an
injected binary. `InputTopology` is variable-length and string-keyed, so it
cannot be a fixed wire struct without a redesign.

**Decided: `bincode`.** Hook binary size wins over debuggability — the decoder
is linked into every injected DLL, and `serde_json` is a large dependency to
carry for one low-rate message.

---

## 15. Pointer architecture

### 15.1 Five sources, three native modalities

| Source | Native modality | Native units | Today |
| --- | --- | --- | --- |
| Host mouse | Relative | viewer pixels (also has an absolute cursor) | `PointerRoutingState::send` → `map_pointer` → `PointerInput` (pixels) + `normalized_position` → gamepad axes 6/7, DirectInput, Win32 |
| Wii Remote IR camera | AbsoluteDevice | blob centroid over the 1023×767 camera field, X mirrored | `ir_pointer_position` → 0..1 → `pointer_axis` → axes 6/7 |
| Wii Remote air mouse | Relative (integrated to absolute) | gyro rad/s, scaled by `POINTER_GAIN = 0.78` | `AirMouse::next_pointer_position` → 0..1 clamped → axes 6/7 |
| Switch 2 optical | Relative | sensor counts | accumulated at `/4096.0`, clamped 0..1 → axes 6/7 |
| SDL touchpad | AbsoluteDevice | pad-local normalized | `update_touchpad_pointer` → `normalized_pointer_axes` |

Every one of them is currently flattened to "normalized 0..1 on gamepad axes 6
and 7", which erases the modality distinction that actually matters. Two bugs
fall out of that flattening and must not be reproduced:

- `Sdl3ProviderWorker::pointer_for` falls back to the host mouse position for
  **every** slot (`.or(pointer.position)`), so four players share one cursor.
  Its doc comment claiming "the first virtual gamepad" is stale.
- The Switch 2 optical sensor is a desk mouse. Integrating it into a clamped
  unit box gives it invisible walls at the edges and no relationship to content
  bounds or DPI.

### 15.2 Projection is conditional, not universal

Emulators split into two groups, and the split decides whether host-side
projection runs at all:

- **Emulators that render their own cursor** — Dolphin, and most that expose an
  in-game pointer. They need **no host projection and no correction, including
  for the host mouse**, which reaches them as SDL relative motion and is
  integrated by the emulator against its own screen geometry. Feeding these a
  projected absolute position is worse than useless: it double-corrects.
- **Emulators that do not render a cursor** — azahar, melonDS, anything where the
  pointer is a touch on a specific emulated screen. Here *every* pointer source
  needs absolute mapping onto the displayed content, because the emulator has no
  cursor of its own to move.

So there are three modalities a consumer can ask for:

```rust
pub enum PointerMode {
    None,
    Relative,
    AbsoluteDevice,
    AbsoluteScreen,
}
```

- `Relative` — deltas. Requires no knowledge of bounds, no calibration, no
  compositing plan. Dolphin's case.
- `AbsoluteDevice` — the device's own absolute field, untouched: raw IR camera
  coordinates, raw touchpad coordinates. The emulator does its own mapping.
  Dolphin's IR axes and raw HID passthrough are both this.
- `AbsoluteScreen` — projected onto the displayed content by the host. azahar
  and melonDS touch.

**`map_pointer` runs only for `AbsoluteScreen`.** That is a much smaller blast
radius than treating projection as the universal path, and it means the
calibration problem only has to be solved for the case where the host is
genuinely the only party that can solve it.

### 15.3 `PointerSample` at the HAL boundary

The HAL reports the device's native form and never normalizes to a screen it
cannot see.

```rust
pub enum PointerSample {
    None,
    Relative { delta: [f32; 2], counts_per_unit: f32 },
    Angular { position: [f32; 2], field_of_view: [f32; 2], confidence: f32 },
    Surface { position: [f32; 2], contact: bool },
}
```

| Producer | Variant | Notes |
| --- | --- | --- |
| Switch 2 optical, any HID mouse | `Relative` | `counts_per_unit` is the sensor's counts per inch, replacing the magic `/4096.0`. The accumulate-and-clamp in `JoyCon2*Decoder::optical_position` is **deleted** — a mouse is relative, and the consumer decides whether to integrate it. |
| Wii Remote IR camera | `Angular` | `position` is the blob centroid normalized over the camera field; `field_of_view` is that camera's angular extent (≈33°×23°); `confidence` is the visible-blob count over four. |
| Wii Remote air mouse | `Angular` | `position` from integrated yaw/pitch; `field_of_view` states the angular window `POINTER_GAIN` currently bakes in. Low `confidence`, so a consumer can prefer IR when both are live. |
| SDL touchpad | `Surface` | pad-local, `contact` from the touch event. |
| Host mouse | not a HAL sample | the viewer's cursor and its raw deltas are inputs to the pointer stage, not a device report. |

`confidence` is what lets the IR→air-mouse handoff stop being a silent
`or_else` inside `WiimoteDecoder::decode`. The consumer can cross-fade or hold,
and the GUI can show which is live.

The `AirMouse` integrator stays in `polymer-input-bluetooth` — gyro bias
estimation, gravity reference, and MotionPlus fast/slow scaling are device
physics. What leaves is the final `0.5 + angle * GAIN` step, which is screen
geometry pretending to be device physics.

### 15.4 Capability, and the plugin decides

Each device advertises what its pointer can do; the emulator plugin, which is
the only party that knows what the emulator expects, chooses the mode.

The `POINTER_*` bits of `Capabilities` (§4.1) carry this. `POINTER_ANGULAR` and
`POINTER_SURFACE` describe the native absolute form; `POINTER_ABSOLUTE` is their
union, so a device that reports either can serve `AbsoluteDevice`.
`CALIBRATE_POINTER` covers the sensor bar and IR controls.

Surfaced to plugins on `ControllerMapping.device`:

```ts
export type ControllerDevice = {
    readonly id: string;
    readonly name: string;
    readonly profile: string | null;
    readonly capabilities: readonly Capability[];
};

export type Capability =
    | "rumble" | "hdRumble" | "playerLed" | "homeLed"
    | "accelerometer" | "gyroscope" | "orientation" | "touchpad" | "battery"
    | "rawHid" | "recenter"
    | "pointerRelative" | "pointerAngular" | "pointerSurface" | "pointerContact"
    | "calibrateStickCenter" | "calibrateStickRange" | "calibrateDeadZone"
    | "calibrateTrigger" | "calibrateMotionBias" | "calibrateMotionScale"
    | "calibratePointer";
```

The mode is **per-port configuration**, not a separate list on the launch
result. It travels with the port everywhere a port already travels:

```rust
pub enum PointerMode { None, Relative, AbsoluteDevice, AbsoluteScreen }

pub struct ControllerPort {
    pub group: String,
    pub index: usize,
    pub layouts: Vec<String>,
    pub pointer: PointerMode,
}
```

| Type | Change | Direction |
| --- | --- | --- |
| `polymer-controllers::ControllerPort` | `+ pointer: PointerMode` | plugin declares, statically, from `ports(console)` |
| `ControllerMapping` | `+ pointer: PointerBinding` | mode from the plugin, source from the user |
| `PortDescriptor` | `+ pointer` | host → frontend, for display |
| `PortBinding` (§9) | `+ pointer` | host → hook |

`ports(console)` gives the static default. `onControllerChanged` may override it
per port, which is what makes the capability-driven choice below possible at
all — the right mode depends on which device is *currently* assigned, and that
is not known at `ports()` time.

`LaunchPortRequest` is unchanged: the frontend chooses a pointer *source*
(§15.9) through the controller UI, never a mode. Mode is the plugin's.

Worked examples:

```ts
// dolphin.ts — Dolphin draws its own cursor.
// A real Wii Remote's IR goes through untouched so Dolphin's IR axes see raw
// camera coordinates; everything else is relative motion into Dolphin's cursor.
const pointerMode = (mapping: ControllerMapping): PointerMode =>
    mapping.target.group !== "wii" ? "none"
    : mapping.device?.capabilities.includes("pointerAngular") ? "absoluteDevice"
    : "relative";

// azahar.ts — the 3DS bottom screen is a touchscreen with no cursor.
// Every source must land on the displayed content, so the host projects.
const pointerMode = () => "absoluteScreen";
```

Requested mode and native modality do not always agree. The conversion is
explicit, stateful where it has to be, and named:

| Requested ↓ / Native → | `Relative` | `Angular` | `Surface` |
| --- | --- | --- | --- |
| `Relative` | passthrough | differentiate | differentiate |
| `AbsoluteDevice` | *unavailable* | passthrough, device units | passthrough, pad units |
| `AbsoluteScreen` | accumulate, clamp to content | calibrate, then project | map pad rect → content rect |

`AbsoluteDevice` from a `Relative`-only device is the one genuinely impossible
cell: a desk mouse has no absolute field of its own. The runtime rejects the
binding at launch with a clear error rather than silently substituting
`AbsoluteScreen`, because substituting would put a cursor somewhere the emulator
does not expect.

### 15.5 The projector — `AbsoluteScreen` only

New module `crates/consumer/polymer-renderer/src/pointer/mod.rs`, because it
needs `Presentation` and `CompositingPlan` and nothing else does.

```rust
pub struct PointerProjector {
    cursor: Point,
    calibration: PointerCalibration,
    target: PointerTarget,
    sensitivity: f32,
}

pub enum PointerTarget {
    HitTest,
    Surface(SurfaceSelector),
}

pub struct PointerProjection {
    pub pointer: PointerInput,
    pub normalized: [i16; 2],
    pub surface_id: u64,
}

impl PointerProjector {
    pub fn accept(&mut self, sample: &PointerSample, bounds: Rect, now: Instant);
    pub fn set_cursor(&mut self, point: Point);
    pub fn recenter(&mut self);
    pub fn project(&mut self, presentation: &Presentation, plan: &CompositingPlan)
        -> Option<PointerProjection>;
}
```

`accept` produces a cursor in viewer presentation space per the conversion table
above. `project` then calls the existing `map_pointer` **unchanged**, returning
both coordinate conventions the current code already computes: the pixel
position for `PointerInput` and the pixel-endpoint normalized position for the
gamepad axes. `PointerRoutingState` becomes the host-mouse driver of one
`PointerProjector` rather than the only projection path in the system.

`PointerTarget::Surface` is the one addition `map_pointer` lacks: pin a pointer
to a surface and clamp to it, instead of hit-testing. Without it a Wii Remote
aimed above a 3DS bottom screen delivers touch events to the top screen.

### 15.6 Sensor bar and IR setup is feedback

A Wii Remote's IR camera does not work until it is programmed, and programming
it is a sequence of acknowledged output reports. `WiimoteModel::initialize_ir_camera`
already does exactly this today:

| Output report | Effect |
| --- | --- |
| `0x13` `IR_ENABLE_ID` | camera power |
| `0x1a` `IR_ENABLE_2_ID` | camera clock |
| `0x16` i2c write `0xb00030 = 0x01` | enter configuration |
| `0x16` i2c write `0xb00000` (9 bytes) | **sensitivity block 1** |
| `0x16` i2c write `0xb0001a` (2 bytes) | **sensitivity block 2** |
| `0x16` i2c write `0xb00033` | **camera mode** (basic / extended / full) |
| `0x16` i2c write `0xb00030 = 0x08` | commit |
| `0x12` `REPORT_MODE_ID` | report mode `0x37` |

This is mechanically identical to `set_rumble` (report `0x11` via `M::set_led`)
and `set_player_led` (the same report) — same `ControllerSession`, same
transport, same acknowledgement handshake. It is stateful device command
traffic, so it belongs on the feedback channel, as `Feedback::Calibrate`.

```rust
pub struct PointerSetup {
    pub sensor_bar: SensorBarPosition,
    pub sensitivity: IrSensitivity,
    pub mode: IrMode,
}

pub enum SensorBarPosition { Above, Below }
pub enum IrSensitivity { Level1, Level2, Level3, Level4, Level5 }
pub enum IrMode { Basic, Extended, Full }
```

`PointerSetup` is the `pointer` field of `Calibration` (§16.3) and reaches the
backend as `Feedback::Calibrate`. Sensor bar sensitivity and position *are*
calibration data; they differ from stick calibration only in that the backend
realizes them as output reports rather than a software transform.

The backend fans the struct out to whichever mechanism each field needs:
`sensitivity` and `mode` become the register blocks above, while `sensor_bar`
parameterizes the decoder's vertical mapping. Both are backend-internal
concerns; the hub latches one value and does not care how it is applied.

Two concrete changes in `polymer-bluetooth`:

- `CAMERA_SENSITIVITY_BLOCK_1_ADDRESS` / `_BLOCK_2_ADDRESS` currently write the
  hardcoded `[0x02, 0x00, 0x00, 0x71, 0x01, 0x00, 0xaa, 0x00, 0x64]` and
  `[0x63, 0x03]`, and `CAMERA_MODE_ADDRESS` writes a constant
  `CAMERA_BASIC_MODE`. These become a table indexed by `IrSensitivity` and a
  value from `IrMode`.
- `ControllerSession` gains
  `async fn set_pointer_setup(&mut self, transport, setup) -> Result<()>`, and
  `WiimoteSession` stores the setup so `synchronize` reprograms from it rather
  than from constants. Without this the latch in §4.5 has nothing to latch
  *into*, and every reconnect silently reverts the user's sensitivity.

`PointerCapabilities::SENSOR_BAR` tells the GUI whether to offer the control.
Only the Wii Remote consumes it today.

Persistence is **localStorage on the shell side**, keyed by device id — no Rust
storage schema and no `polymer-library` table:

```ts
Polymer.controllers.configurePointer(deviceId, {
  sensorBar: "above", sensitivity: 3, mode: "basic",
});
```

The shell pushes the stored value on `deviceconnected`; the hub's latch covers
reconnects the shell does not observe. That also settles the earlier open
question about per-display calibration persistence: it is a localStorage key and
can gain a display component later without a migration.

Full geometric calibration stays deferred. `PointerCalibration` is defined as a
homography with an identity default so a four-point corner capture is additive:

```rust
pub struct PointerCalibration { matrix: [[f32; 3]; 3] }

impl PointerCalibration {
    pub const IDENTITY: Self;
    pub fn from_corners(observed: [[f32; 2]; 4], target: Rect) -> Option<Self>;
    pub fn project(&self, position: [f32; 2], field_of_view: [f32; 2]) -> Point;
}
```

Ship identity plus the sensor bar offset. Because `AbsoluteScreen` is now only
required by touch-style emulators — where the host mouse is the overwhelmingly
common source and needs no calibration at all — four-point capture is a
refinement for a niche case, not a blocker.

### 15.7 Wiring the projected result back

Only in `AbsoluteScreen` mode. The hub keeps one override slot per index:

```rust
impl InputHub {
    pub fn pointer_sample(&self, index: PlayerIndex) -> Option<PointerSample>;
    pub fn set_pointer_override(&self, index: PlayerIndex, pointer: Option<PointerReport>);
}
```

The viewer runs projectors in the present/input loop it already has and writes
the override; the hub merges it into `DeviceState.pointer` at publish time — a
per-index field copy, no allocation, and no re-entrancy into the viewer from a
backend's publish thread.

This couples the pointer to a presented frame by one frame. That is **correct,
not a compromise**: the projection is defined against a specific frame's
composite placement, and `PointerInput::frame_number` already records which one.
The host mouse path has exactly this property today.

In `Relative` and `AbsoluteDevice` modes there is no override and no projector.
The sample is converted in place and published directly, which is why Dolphin
pays nothing for machinery it does not use.

Rejected alternatives, for the record: a synthetic `BackendId::VIRTUAL` pointer
device (invents a device that does not exist, muddies assignment); a
`dyn PointerProjector` callback inside `InputHub::publish` (puts frame-coupled
viewer state on a backend thread and makes the hub depend on the renderer).

### 15.8 `PointerReport` on the wire

```rust
#[derive(Clone, Copy, Default, PartialEq)]
pub struct PointerReport {
    pub position: [i16; 2],
    pub delta: [i16; 2],
    pub flags: PointerFlags,
    pub kind: PointerKind,
}

bitflags! {
    pub struct PointerFlags: u8 {
        const PROJECTED = 1 << 0;
        const DEVICE    = 1 << 1;
        const CONTACT   = 1 << 2;
        const NATIVE    = 1 << 3;
        const HOST      = 1 << 4;
    }
}

pub enum PointerKind { None, Mouse, IrCamera, AirMouse, Optical, Touchpad }
```

`position` keeps the existing `normalized_pointer_axes` encoding, so gamepad
axes 6 and 7 stay bit-identical for consumers that already read them. `delta`
carries relative motion so `Relative` mode needs no differentiation downstream.
`PROJECTED` versus `DEVICE` tells a hook which coordinate space it is holding —
a mapping should refuse to bind a `DEVICE`-space pointer to a screen-space
control, and vice versa.

### 15.9 Two orthogonal axes

Source selection and modality are independent and both are needed:

```rust
pub struct PointerBinding {
    pub source: PointerSource,
    pub mode: PointerMode,
}

pub enum PointerSource {
    None,
    DeviceNative,
    HostMouse,
    DeviceThenHostMouse,
}
```

- **Source** — user and GUI concern. Which physical pointer drives this player.
  `DeviceThenHostMouse` is what today's `.or(pointer.position)` fallback was
  reaching for, but now **per player**, so four players no longer share one
  cursor. `HostMouse` is the explicit `PortConfiguration::force_mouse` case.
- **Mode** — plugin and emulator concern (§15.4). What the emulator expects.

Both travel in `PortBinding` on the topology control channel (§9).

### 15.10 Raw HID passthrough is `AbsoluteDevice` taken to its limit

Under `RemapMode::Passthrough` with a raw-HID binding — Dolphin real Wii
Remote — the IR data reaches Dolphin inside the untouched HID report and Dolphin
runs its own IR math. No projector, no `PointerReport`, no conversion. Mode is
forced to `None` on the gamepad plane for that port.

This is consistent rather than exceptional under the new model: passthrough and
`AbsoluteDevice` differ only in transport. Both say "the emulator owns the
pointer geometry". The sensor bar setting still applies, but it is *Dolphin's*
setting in that path, not ours — worth surfacing in the GUI so the value is not
configured twice with different answers.

### 15.11 DS / 3DS touch is the same path

For azahar and melonDS the pointer is a touch on one emulated screen.
`map_pointer` already resolves this: the multi-surface hit test selects the
bottom screen and `presentation_mapping` inverts into its coordinates, including
the `full_window_graph_mappings_letterbox_to_the_displayed_aspect` case the
renderer already handles for azahar's separate windows.

With `PointerTarget::Surface` pinning, a Wii Remote or a Switch 2 mouse driving a
3DS bottom screen runs through byte-identical code to the host mouse. No
emulator-specific pointer path is required.

### 15.12 Relative-as-cursor versus relative-as-axis

Two behaviours share the word "relative":

- **Cursor** — accumulate and clamp. Needs content bounds, so it is viewer-side,
  in `PointerProjector::accept`, and only in `AbsoluteScreen` mode.
- **Axis** — decay to zero so mouse-look returns to centre.
  `relative_mouse_weight` with its 16 ms hold / 50 ms release ramp, currently in
  `provider/sdl3/mod.rs::PointerSample::relative`.

The decay is a *mapping* behaviour and moves to
`polymer-input-layout::mapping` alongside `AxisInput::MouseDelta`, fed from
`HostInputEvent::MouseMotion` (host mouse) or `PointerReport::delta` (device
mouse). Preserve the hold/release constants; they were tuned.

### 15.13 Settled

- **`PointerMode` is per-port configuration** (§15.4), carried on
  `ControllerPort`, `ControllerMapping`, `PortDescriptor`, and `PortBinding`.
  It touches the configuration materialization path, which is the cost of
  having it travel with the port rather than beside it.
- **`PointerMode::Relative` is the default** when a plugin declares nothing. A
  wrong `AbsoluteScreen` produces a stuck or teleporting cursor; a wrong
  `Relative` is merely imprecise. `#[derive(Default)]` on `PointerMode` must
  therefore name `Relative`, not the first variant — spell it explicitly so a
  later reorder cannot silently change it.
- **Pointer capabilities live in the one `Capabilities` bitfield** (§4.1), not
  a separate type.

## 16. Calibration

### 16.1 There is currently none for Bluetooth

SDL3 arrives pre-calibrated: it applies `gamecontrollerdb` mappings and its own
internal axis normalization before we see an axis, so the SDL backend inherits
usable centres, ranges, and dead zones for free.

The Bluetooth path has nothing. Everything is a compiled-in constant:

| Quantity | Today | Correct source |
| --- | --- | --- |
| Stick centre and range | `decoder::normalize_stick` — `(raw - 2048) * 32767 / 2047`, i.e. centre 2048, range ±2047, for every Switch-family device ever made | per-unit factory calibration in SPI flash |
| Accelerometer scale | `/ 4096.0 * GRAVITY` in `SwitchProDecoder` | factory scale + zero-point |
| Accelerometer zero | not corrected at all | Wii Remote EEPROM `0x0016`; Switch SPI |
| Gyroscope bias | `AirMouse::update_stationary_bias` estimates it at runtime, then discards it on disconnect | factory value, refined at runtime, persisted as a seed |
| Gyroscope scale | `SLOW_/FAST_RAW_PER_RADIAN_PER_SECOND` constants | factory value |
| Stick dead zone | none | user preference |
| IR sensitivity / mode | hardcoded register blocks (§15.6) | user preference, written to the device |

A drifting stick, an off-centre stick, and a Wii Remote whose accelerometer
reads 0.15 g at rest are all currently unfixable, and there is no recalibration
path at all.

### 16.2 One concept, three realizations

The user's observation is the design: sensor bar sensitivity, stick dead zone,
stick centre, and gyro bias are the same *kind* of thing — latched per-device
state that changes how the device behaves or how its reports are read. What
differs is only how a backend realizes it:

| Field | SDL3 backend | Bluetooth backend |
| --- | --- | --- |
| Stick centre / range | already applied upstream by SDL; ours layers on in software | software, seeded from SPI factory data |
| Stick / trigger dead zone | software | software |
| Accelerometer bias, scale | software | software, seeded from EEPROM or SPI |
| Gyroscope bias, scale | software | software, seeded + refined at runtime |
| IR sensitivity | unsupported | **output reports** — the i2c register blocks of §15.6 |
| IR mode | unsupported | **output reports** |
| Sensor bar position | unsupported | software, in the decoder's vertical mapping |

So the caller says *what* it wants and the HAL decides *how*. That is the
generalization, and it is why calibration rides the existing host → HAL command
path rather than getting its own: the `PointerSetup` of §15.6
becomes one field of a general `Calibration`, and the IR register writes stop
being a special case.

### 16.3 Shape

```rust
#[derive(Clone, Default, PartialEq)]
pub struct Calibration {
    pub sticks: [Option<StickCalibration>; 2],
    pub triggers: [Option<TriggerCalibration>; 2],
    pub motion: Option<MotionCalibration>,
    pub pointer: Option<PointerSetup>,
}

#[derive(Clone, Copy, PartialEq)]
pub struct StickCalibration {
    pub center: [i16; 2],
    pub min: [i16; 2],
    pub max: [i16; 2],
    pub dead_zone: f32,
    pub outer_dead_zone: f32,
}

#[derive(Clone, Copy, PartialEq)]
pub struct TriggerCalibration { pub min: u16, pub max: u16, pub dead_zone: f32 }

#[derive(Clone, Copy, PartialEq)]
pub struct MotionCalibration {
    pub accelerometer_bias: [f32; 3],
    pub accelerometer_scale: [f32; 3],
    pub gyroscope_bias: [f32; 3],
    pub gyroscope_scale: [f32; 3],
}
```

Every field is `Option` so a partial update is expressible without a
read-modify-write race: setting a dead zone must not clobber a centre captured
by a concurrent recalibration.

Note what is **not** here: no factory values, no effective values, no device
units. `Calibration` is the user layer and nothing else — see §16.4.

Inside `polymer-input-bluetooth`, `normalize_stick`'s constants become a
stated per-profile factory value rather than arithmetic buried in a function:

```rust
const SWITCH_STICK_FACTORY_DEFAULT: StickCalibration = StickCalibration {
    center: [2048, 2048],
    min: [1, 1],
    max: [4095, 4095],
    dead_zone: 0.0,
    outer_dead_zone: 0.0,
};
```

It reuses the same struct for convenience, but it never crosses the HAL
boundary. Making it a value rather than an expression is what lets a real SPI
read replace it without touching decode.

### 16.4 Calibration is write-only at the HAL boundary

Factory calibration — Switch SPI flash, Wii Remote EEPROM `0x0016`, SDL's own
internal normalization, or a profile default where none has been implemented —
is **opaque to and internal to the backend crate**. It appears in no trait, no
snapshot, and no message. A backend reads it if it can, composes it with the
user layer it has been given, and publishes reports that are already calibrated.

```rust
pub trait InputBackend {
    fn set_calibration(&self, device: &DeviceId, calibration: &Calibration)
        -> Result<(), BackendError>;
}
```

That is the entire calibration surface on the HAL. There is no `calibration()`
read, no `CalibrationSnapshot` crossing the boundary, and no bypass control.

The consequences are worth stating because they are what make the rest of this
section simple:

- **Nothing above the HAL sees raw device units.** A stick's centre in SPI flash,
  a Wii Remote's accelerometer zero, and SDL's internal axis table never leave
  the crate that knows how to read them. Adding real factory reads later changes
  no API and no caller.
- **The hub owns the only readable layer.** It already latches the user
  calibration for reconnect replay (§4.5); that latch *is* the settings state.
  A UI reading a dead-zone slider reads the hub, not a device.
- **There is no effective-value read**, so the UI cannot show "factory says X".
  It shows the user's adjustments and a reset, which is what a calibration UI
  actually needs. Labelling an SDL device's correction as "handled by SDL" is
  honest; inventing numbers for it was not.

`Calibration` therefore carries only the user layer. `CalibrationSnapshot`,
`factory`, and `effective` from earlier drafts are deleted.

What the device *supports* is still needed, so the GUI offers an IR sensitivity
control only for a Wii Remote and a dead-zone slider only where one means
something. That is a capability, so it goes on `DeviceDescriptor` alongside the
others rather than being a read method:

```rust
bitflags! {
    pub struct CalibrationFields: u16 {
        const STICK_CENTER    = 1 << 0;
        const STICK_RANGE     = 1 << 1;
        const STICK_DEAD_ZONE = 1 << 2;
        const TRIGGER_RANGE   = 1 << 3;
        const MOTION_BIAS     = 1 << 4;
        const MOTION_SCALE    = 1 << 5;
        const POINTER         = 1 << 6;
    }
}
```

### 16.5 Addressing: per device, not per player

This is the one place calibration diverges from the rest of the feedback
channel. `ListenerHandle::feedback` is index-addressed and fans out to a
composite's members — both halves of a Joy-Con pair should buzz. Calibration
must not fan out: the two halves have *different* stick calibrations, and
broadcasting one to both is silently wrong.

```rust
impl ListenerHandle {
    pub fn calibration(&self, device: &DeviceId) -> Calibration;
    pub fn calibrate(&self, device: &DeviceId, calibration: &Calibration)
        -> Result<(), HubError>;
    pub fn reset_calibration(&self, device: &DeviceId) -> Result<(), HubError>;
}
```

`calibration()` returns the hub's latched user layer. It is a host-side read of
host-side state and involves no device traffic.

Below the listener boundary everything is device-addressed anyway —
`InputBackend::apply_feedback` already takes a `DeviceId` — so this is a
listener-API distinction, not a second transport. The index-addressed
`feedback()` resolves to one or more device commands; `calibrate()` names its
device directly.

### 16.6 Shared transform, backend-owned application

The software half of the realization table must not be written per backend.
`polymer-input` provides the transform as a pure function; the backend calls
it, having already folded in whatever factory values it holds:

```rust
impl Calibration {
    pub fn correct(&self, state: &mut DeviceState);
    pub fn shape(&self, state: &mut DeviceState);
}
```

Two stages, applied in that order, and the split matters for §16.7:

- **`correct`** — centre, range, bias, scale. Makes the device report truthfully.
- **`shape`** — dead zone and outer dead zone. Applies a preference on top.

The Bluetooth backend calls both after decode. The SDL backend calls them with
stick and trigger fields cleared for gamepad axes SDL has already normalized,
and set for the raw joystick axes SDL has not. A backend that realizes a field
in hardware — the Wii Remote's IR registers — leaves that field out of the
software pass entirely.

The HAL stays in charge of realization, which was the requirement, without
duplicating dead-zone and range math three times.

### 16.7 Recalibration without reads

Recalibration is interactive: hold the sticks centred, sweep them to the rim,
leave the controller still on a flat surface.

```rust
pub enum CalibrationKind { StickCenter, StickRange, TriggerRange, MotionBias }

impl ListenerHandle {
    pub fn begin_calibration(&self, device: &DeviceId, kind: CalibrationKind)
        -> Result<CalibrationCapture, HubError>;
}

impl CalibrationCapture {
    pub fn progress(&self) -> f32;
    pub fn finish(self) -> Result<(), CalibrationError>;
    pub fn cancel(self);
}
```

Because reports arriving at the capture are already calibrated, a capture
observes and produces a **correction in output space**, not an absolute value in
raw device units:

| Kind | Observation | Correction |
| --- | --- | --- |
| `StickCenter` | resting position, ideally `[0, 0]` | subtract the observed offset |
| `StickRange` | extremes of a rim sweep, ideally full scale | scale by the shortfall |
| `TriggerRange` | released and fully pressed values | offset and scale |
| `MotionBias` | gyro at rest, ideally `[0, 0, 0]` | subtract the observed rate |

The hub then **composes** the correction with its latched user layer rather than
replacing it, and writes the composed absolute value down. Replacement would be
a bug: a second centre capture on a now-correct stick observes zero offset, and
replacing with zero would discard the first correction and reinstate the drift.
Composition converges instead — which is also the behaviour a user expects from
pressing "calibrate" twice.

This is why the capture needs no raw access, and why `set_calibration_bypass`
from the earlier draft is deleted.

One real hazard survives and is handled by writing rather than reading. If a
dead zone is applied to the reports the capture observes, a drifting stick reads
exactly `[0, 0]` at rest and a centre capture can never see the drift it exists
to correct. So `begin_calibration` suppresses shaping for the duration:

1. `begin_calibration` writes the latched calibration with `dead_zone` and
   `outer_dead_zone` zeroed. `correct` still applies; `shape` becomes identity.
2. The capture observes corrected-but-unshaped reports and accumulates.
3. `finish` composes, restores the user's dead zones, and writes once.
4. `cancel` — and the capture's `Drop` — restores without composing.

Every step is a write. The hub can restore because it holds the latch, so no
read of the device is required at any point. A crash mid-capture is covered by
the reconnect replay, which rewrites the latched value including the dead zones.

### 16.8 Persistence

Same mechanism as the pointer setup: **localStorage on the shell side**, keyed
by device id, pushed on `deviceconnected` and on change, with the hub's
`Latched` retention covering reconnects the shell does not observe.

```ts
Polymer.controllers.calibrate(deviceId, {
  sticks: [{ center: [12, -4], min: [-31000, -30800], max: [31200, 30900],
             deadZone: 0.08, outerDeadZone: 0.02 }, null],
  pointer: { sensorBar: "above", sensitivity: 3, mode: "basic" },
});
Polymer.controllers.getCalibration(deviceId);   // the user layer, from the hub
Polymer.controllers.resetCalibration(deviceId);
```

Only the user layer is ever persisted, because it is the only layer that exists
outside a backend. `configurePointer` from §15.6 collapses into `calibrate` with
only the `pointer` field set; keep it as a convenience wrapper in JS if the
settings UI wants it.

If this outgrows localStorage — it is larger and more structured than the
pointer setup alone — moving it to `polymer-library` changes no Rust API,
because the hub never sees the storage.

### 16.9 Remaining scope note

- **Where do the deferred factory reads live inside each backend?** They are
  opaque either way, but the placement differs: Switch SPI reads belong in
  `polymer-bluetooth`'s `ControllerSession::synchronize` alongside the existing
  MotionPlus and IR init, since that is already an acknowledged-write sequence
  on the connect path; the Wii Remote EEPROM read at `0x0016` likewise. The
  decoders in `polymer-input-bluetooth` then receive the values rather than
  reading them. **Recommendation:** device traffic in `polymer-bluetooth`,
  application in `polymer-input-bluetooth`, matching the existing split.
- **Profile defaults until then — decided.** `normalize_stick`'s centre 2048 /
  range ±2047, the `/ 4096.0 * GRAVITY` accelerometer scale, and the
  `*_RAW_PER_RADIAN_PER_SECOND` gyro constants become a table inside
  `polymer-input-bluetooth` **keyed by `(vendor_id, product_id)`**, not by
  profile id. A vid/pid pair is what the eventual on-board factory read is also
  keyed by, so the interim table and its replacement address devices the same
  way, and the NSO controllers and GameCube adapter can carry values that differ
  from the Pro Controller's without inventing new profile ids.
- **Runtime-estimated bias is written back — decided.**
  `AirMouse::update_stationary_bias` converges over a session and currently
  discards the result. It is persisted as a user value with a visible reset,
  and **only** for `CALIBRATE_MOTION_BIAS` — never for range or centre, where a
  bad sample is not self-correcting.

## 17. Proposed test coverage — **ask before writing**

Not to be implemented without sign-off.

1. `hub/assignment.rs` — index retention across disconnect / reconnect, explicit
   assignment displacing an auto-assignment, composite fan-out of feedback.
2. `hub/assignment.rs` auto-composition — the §14.6 rule at each count: one
   left + one right composes; two of either leaves all four individual; an
   explicit `decompose` is not undone by a reconnect within the session.
3. `FeedbackClass` arbitration — `Actuation` dropped from a non-owner while
   `Latched` is accepted; a `Latched` value replayed on `Connected`; a
   `Latched` value surviving a feedback-priority change.
4. `polymer-input-layout::Mapping` — `compile` + `apply` across the three
   projection modes, reproducing the existing
   `inverted_axis_saturates_at_the_positive_limit` and
   `gamepad_binding_survives_keyboard_mouse_fallback` cases.
5. `polymer-input-ipc` — the four existing `polymer-ipc/src/input.rs` tests
   retargeted: handshake, protocol mismatch, multi-subscriber broadcast, late
   subscriber replay. Plus one asserting the input frame no longer costs
   `PAYLOAD_SIZE`.
6. Raw HID — port `capture_mask_preserves_accelerometer_low_bits_and_hides_home`
   and `captured_speaker_data_retains_the_rumble_bit` onto `RawHidFilter`.
7. Pointer modality conversion (§15.4 table) — differentiating an `Angular`
   sample into `Relative`, and the rejected `Relative` → `AbsoluteDevice` cell
   producing a launch error rather than a substitution.
8. `PointerProjector` — `Relative` accumulation clamping to content bounds
   rather than a unit box; `Angular` under identity calibration plus each
   `SensorBarPosition`; `PointerTarget::Surface` pinning to the bottom screen
   while the cursor sits over the top screen. The four existing
   `polymer-renderer/src/input.rs` pointer tests must keep passing unchanged —
   they are the regression net for the projection itself.
9. `Calibration` — a partial update leaving other fields intact; `correct` then
   `shape` with an off-centre stick and a radial dead zone; and the composition
   property of §16.7: a second capture on an already-corrected stick must
   converge rather than discard the first correction. That last one is the
   defect most likely to ship silently, because it only shows up on the second
   recalibration.
10. Static assertions (preferred over tests per `AGENTS.md`): `GamepadButton`
   wire order vs `sdl::BUTTONS`; `size_of::<WireDeviceState>()` bound;
   `DeviceState: Copy`.

## 18. Verification

- `plugins/dolphin` Wii with a real Wii Remote exercises raw HID, the topology
  channel, `RemapMode::Passthrough`, and assignment at once. Run it before and
  after.
- A Switch Pro over Bluetooth plus an XInput pad over SDL3 in one session
  exercises two backends, index assignment, and rumble arbitration.
- **Bonded reconnect without a session.** Bond a Joy-Con pair, restart the app,
  and confirm both halves reconnect on their own and re-compose into one player
  at the same index — with no pairing session opened and no L+R gesture. Then
  repeat with four Joy-Cons and confirm they stay individual (§14.6). This is
  new behaviour, not a regression check.
- Keyboard-only play on any layout exercises the relocated keyboard projection.
- Overlay↔game priority flips exercise §14.10.
- **Pointer matrix.** Split by modality, because the two halves land in
  different migration steps and fail differently.
  - **`Relative` / `AbsoluteDevice` (step 10, no projector).** Dolphin Wii with
    an emulated Wii Remote, driven by (a) host mouse through SDL relative,
    (b) Wii Remote IR as `AbsoluteDevice` into Dolphin's IR axes, (c) Wii Remote
    air mouse with the sensor bar covered, (d) Switch 2 Joy-Con optical as a
    plain relative mouse. Confirm none of these enters `map_pointer`.
  - **`AbsoluteScreen` (step 11, projector).** azahar 3DS bottom-screen touch,
    separate-windows layout, driven by the same four sources. This is the
    `PointerTarget::Surface` and letterbox-inference case.
  - **Passthrough.** Dolphin Wii with raw HID to confirm the projector is
    bypassed and Dolphin's own IR math is untouched (§15.10).
  - **Sensor bar and IR setup.** Flip `SensorBarPosition` and each
    `IrSensitivity` level with the bar physically above and below the display.
    Then verify the latch (§4.5, §15.6) against the two paths that re-run
    `synchronize`: power-cycle the Wii Remote to force a reconnect, and enter
    then leave raw HID passthrough via `set_external_ownership`. The
    configuration must survive both, and must survive an overlay↔game
    feedback-priority flip.
  - **Two players with pointers simultaneously**, to confirm the shared-cursor
    defect in `pointer_for` is gone.
- **Calibration.** A Switch Pro or Joy-Con with a visibly off-centre stick:
  recalibrate centre and range with a non-zero dead zone already configured —
  this is the case that fails if `begin_calibration` does not suppress shaping
  (§16.7), because a drifting stick reads exactly centre through a dead zone.
  Then recalibrate a second time and confirm the correction converges rather
  than reverting. Confirm both survive a reconnect. Then a Joy-Con pair, to
  confirm calibrating one half does not overwrite the other — the fan-out
  hazard of §16.5. Finally a Wii Remote left flat on a table, to confirm gyro
  bias converges and that a persisted seed shortens convergence next session.
