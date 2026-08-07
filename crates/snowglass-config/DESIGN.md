# snowglass-config — Design

Revision of the Snowflake configuration/input design, narrowed to what this crate
owns and corrected against the snowglass tree. Source-verified claims are marked
**[V]**; unverified assumptions **[?]**.

---

## 0. Scope

`snowglass-config` is a pure library: **`(definition, values, host facts) → (files, launch descriptor)`**.

It owns:

- parsing definition documents into `Raw*` structs,
- lowering them to a validated arena IR,
- realizing an IR against a value stack, host facts, and an optional resolver runtime,
- emitting configuration bytes and a launch descriptor.

It does **not** own:

- persistence — the caller hands in a `ValueStack` and stores whatever comes back;
- the filesystem — `emit` returns file contents, the host writes them;
- process launch, IPC, or `EmulatorInstance`;
- the JS runtime — only the `ResolverRuntime` trait; `deno_core` lives in the shell.

Dependencies: `serde`, `toml`, `indexmap` (§3.2), `serde_json`,
`blake3`, `smallvec`, and
`snowglass-input` (layout and SDL-projection types). No `tokio`, no `deno_*`,
no `windows`.
`#![forbid(unsafe_code)]`.

**Crate home:** stays at `crates/snowglass-config` rather than moving under
`crates/runtime/`. It depends on nothing in `runtime/` and is meant to be usable
from a host that never launches anything (settings UI, config preview, tests).

### 0.1 Principles

1. **Definitions are documents, not types.** The plugin boundary is a versioned
   schema, not a Rust ABI. No `abi_stable`, no cdylib, no WASM for definitions.
2. **Configuration is a pure function.** No I/O in this crate, at all.
3. **The host owns capability, the plugin owns intent.** A plugin declares that an
   option's domain *is* the set of adapters; it never enumerates them.
4. **Fail at load, not at launch.** Everything statically checkable is checked when
   the definition is lowered, with a diagnostic naming the offending key.
5. **Never destroy user intent.** Unknown keys, hand edits, and stored values whose
   domain member vanished are preserved.
6. **One input vocabulary.** Plugins bind consolespec *binding keys* —
   `button.a`, `analog.stick.left.x+` — the same vocabulary `InputHub`, the SDL
   hooks, the keyboard defaults and the remap database use. There is no second
   namespace and nothing is codegen'd from the spec. consolespec does not spell
   every control out: an inputspec declares a *cluster* (`axes = 2,
   alignment = "left"`, `directions = 4`) and the keys expand from it by a fixed
   rule (§6.5.1). A key is built from its own cluster's fields, so it is stable
   against the device gaining another.

---

## 1. Data flow

```
plugin package (TOML + TS)
        │ parse
        ▼
    Raw* ──lower──► Definition (arena IR, content-hashed)   [once, at load]
                          │
   ValueStack ───────────►│
   HostFacts ────────────►│ realize()                        [per launch / per UI edit]
   dyn ResolverRuntime ──►│
                          ▼
          RealizedDefinition + ResolvedValues + Diagnostics
                          │ emit(existing bytes)
                          ▼
          Realized { config, env, argv, ports, actions, claims }
                          │ serialize
                          ▼
          LaunchDescriptor ──► emulator JS ──► frontend ──► launch op ──► host writes, launches
```

The frontend is a conduit. It receives an opaque JSON object from the emulator's
JS module and hands it to the launch op unchanged; it never interprets an option
path, a target, or an input element.

---

## 2. Plugin package

```
plugins/dolphin/
  plugin.toml                manifest + capability grants
  config/dolphin.toml        definition document
  config/input.toml          input block (may be inlined)
  config/actions.toml        action map (may be inlined)
  config/resolvers.ts        optional; exports are addressed as `resolver:<name>`
  config/dolphin.gen.d.ts    codegen'd types
  support/parse_config.py    regenerates the documents from emulator source
```

```toml
[plugin]
id = "dolphin"
version = "0.4.1"
class = "emulator"
provides = ["configuration", "launch"]

[emulator]
platforms = ["NINTENDO_GCN", "NINTENDO_WII"]

[emulator.configuration]
definition = "config/dolphin.toml"
resolver   = "config/resolvers.ts"    # omit → NoResolvers, no JS runtime is created

[capabilities]
host = ["gpu.adapters", "display.modes", "emulator.build"]
fs   = ["plugin-dir:read"]
```

Capabilities are enforced by the host `CapabilityRegistry`. A resolver cannot
reach anything the manifest did not request.

---

## 3. Definition document

### 3.1 Targets

A target is a place inside a configuration file. Nesting is the structure, and
TOML already has nesting — so the common case declares no relationships at all:

```toml
[targets.gfx]
format = "ini"
path   = "Config/GFX.ini"

[targets.gfx.Hardware]
[targets.gfx.Settings]
[targets.gfx."Color Correction"]
```

The table path **is** the target id: options emit to `"gfx.Hardware"`. The last
path component is the node name, so a section whose INI name is a valid bare key
needs no body whatsoever; quote the key when it is not, and set `node`
explicitly only when the id and the emitted name should differ.

A target carrying `format` and `path` is a file and therefore a root; one without
them is a node inside its enclosing file. Depth is unbounded — XML and JSON
formats nest further than INI's single level.

`parent` exists **only** for the case nesting cannot express: one subtree
reachable from more than one root, such as settings duplicated across RetroArch's
core and global configs. Such a target is declared at the top level and names its
parents:

```toml
[targets.shared-video]
parent = ["retroarch-core", "retroarch-global"]
node   = "video"
```

A shared subtree materializes once per reachable root. Nesting and `parent` are
mutually exclusive on one target: a nested target's parent is where it is
written, and saying it twice can only disagree. So the general structure is a
DAG, the syntax is a tree, and `parent` is the escape hatch — present in a
definition only when a DAG is actually necessary.

Files are the only thing targets address. Environment variables and command-line
arguments are separate, explicit blocks (§3.3) rather than a generic sink on the
target — they have different shapes (flat map, ordered list), different merge
rules, and different owners, and pretending otherwise buys nothing.

### 3.2 Sections and options

Sections are a keyed tree, like targets: the table path is the section key, and
an option's path is that prefix plus its own `key`. Nesting is presentation — a
group inside a page — and it carries one inheritance rule that removes the
repetition worth removing: **an option emits to its section's target unless it
says otherwise.**

```toml
[sections.video]
title  = "Video"
target = "gfx.Settings"

[[sections.video.options]]
key     = "vsync"
title   = "VSync"
type    = "bool"
emit    = { key = "VSync", bool_style = "upper" }   # target inherited
default = false

[[sections.video.options]]
key    = "backend"
title  = "Graphics Backend"
type   = "select"
emit   = { target = "main.Core", key = "GFXBackend" }   # different file, so explicit
domain = { source = "resolver:resolveGraphicsBackend" }  # §3.5

[sections.video.advanced]
title = "Advanced"                       # target inherited from `video`

[[sections.video.advanced.options]]
key     = "adapter"                      # path: video.advanced.adapter
type    = "select"
emit    = { target = "gfx.Hardware", key = "Adapter" }
domain  = { source = "host:gpu.adapters", value = "uuid", label = "name" }
default = { strategy = { first_where = "discrete == true" } }
enabled_when = "video.backend != 'Software'"
```

`target` resolves from the nearest ancestor that declares one; `emit.target`
overrides it per option. A section holding only subsections is a legitimate
grouping and needs no `target` of its own. An option with no `emit` at all is
UI-only — it participates in expressions and defaults but writes nothing.

**An option may also have no `default`**, and that is not the same as having one.
Some emulators do not keep their defaults anywhere a generator can read: PCSX2
sets them in the struct constructors its settings macros wrap, so its definition
has 358 keys and no values. Such an option is *unset* until the user picks
something, and an unset option **emits nothing at all** — the key stays absent
and the emulator applies its own default, which is the only honest thing to write
when we do not know what that default is. Lowering therefore rejects a missing
default only for a static domain, where the first choice would be an arbitrary
pick, and `when_disabled = "default"` is a lowering error on an option that has
none.

Options stay an array of tables rather than becoming keyed tables like their
section. The path would be marginally tidier, but **option order is the one
ordering a reader sees directly** — it is the order of controls on a settings
page — and an array states it in the syntax instead of resting on map iteration
order. Sections keep the keyed form because they have nesting to express, which
is what the table path buys them; options have none.

### 3.2.1 Split section files

A section may replace its body with an `import`, keeping a large definition
readable without changing its section paths:

```toml
# config/pcsx2.toml
[sections.achievements]
import = "achievements.toml"
```

Imports are relative to the file containing the import and must remain inside
the plugin package. The imported TOML remains an ordinary, rooted definition
document, so `config/achievements.toml` repeats the full section path:

```toml
[sections.achievements]
title  = "Achievements"
target = "ini.Achievements"

[[sections.achievements.options]]
key = "enabled"
type = "bool"
emit = { key = "Enabled", bool_style = "lower" }
```

The import is an exact splice point: the imported document must contain only
the named section and its descendants, and the importing section may contain no
field other than `import`. The imported subtree replaces the placeholder at its
position, preserving sibling order. Imports may nest; paths are resolved from
each containing file, and cyclic imports are rejected with their import chain.

`options` remains a reserved subsection name (§4.1): a table under a section is a
subsection, and the `options` array is what holds its controls.

**No graph in the document orders sections.** The target DAG says where a value
is *written*, and `Definition::order` (§4) is a toposort over data dependencies
that says what must *resolve* first — a partial order in which almost every
option is incomparable with almost every other. Neither is a statement about
presentation, and deriving page order from either would mean a settings screen
rearranging itself because two options happened to gain a dependency. So sibling
order is document order, and the section tree supplies the hierarchy.

That costs one dependency: the `Raw*` types hold their children in an
`IndexMap`, since a `HashMap` would discard the order before lowering ever saw
it. It is `indexmap` rather than `toml`'s `preserve_order` feature, which only
governs `toml::Value`; parsing straight into the `Raw*` structs bypasses that
representation entirely.

`repeat` is general — controller ports, memory card slots, per-player blocks —
and expands at lower time into N sections with paths `port.1.mode`,
`port.2.mode`. Nothing downstream knows repetition happened.

### 3.3 Environment and arguments

Two explicit blocks, producing the two non-file outputs directly.

```toml
[env]
QT_QPA_PLATFORM = "windows"                       # literal
DOLPHIN_USER_PATH = { option = "system.user_dir" }  # resolved value

[[argv]]
value = "-b"
when  = "system.batch"

[[argv]]
flag  = "--user"
value = { option = "system.user_dir" }

[[argv]]
flag  = "--config"
value = { option = "video.backend", repr = { mapped = { Vulkan = "GFXBackend=Vulkan" } } }
```

`[env]` is a flat map; each value is a literal or `{ option, repr }`. There is no
per-entry set/append/default-if-unset policy — the merge rule is a host rule, not
a plugin choice (§8.3).

`[[argv]]` entries emit in document order, each optionally gated by a `when`
expression. `flag` emits as a separate argument before the value; omit it for a
bare positional. An entry whose `option` resolved to a disabled or absent value
emits nothing, flag included.

Both blocks reuse `Repr` (§4), so a boolean going to argv renders through the
same named bool style (§3.4) as one going to an INI file.

### 3.4 Bool styles

How a boolean is spelled is a property of the *emulator*, not of each option, so
it is declared once at the top of the document and referenced by name:

```toml
[bool_style.upper]
true  = "True"
false = "False"

[bool_style.numeric]
true  = "1"
false = "0"
```

```toml
emit = { target = "gfx-hardware", key = "VSync", bool_style = "upper" }
```

A definition that writes `True`/`False` in forty places says so once, and a
rename is one edit instead of forty. The host ships no built-in styles: an option
referencing an undeclared name is a lowering error, so a typo cannot silently
fall back to some default spelling. Parsing accepts either member string
case-insensitively, plus the other style's spellings, since existing files may
have been written by the emulator itself.

### 3.5 Domains

| kind | source | plugin code? |
| --- | --- | --- |
| static | inline TOML | no |
| host | `host:gpu.adapters` via capability registry | no |
| resolver | `resolver:resolveGraphicsBackend` → TS module export | yes |

```toml
[[sections.video.options]]
key    = "backend"
type   = "select"
emit   = { target = "main.Core", key = "GFXBackend" }
domain = { source = "resolver:resolveGraphicsBackend" }
```

A build-dependent set is the archetypal resolver domain. Which video backends
exist is a property of *this installation* — Dolphin compiles D3D only on Windows
and Metal only on macOS, and a build can omit any of them — so the honest answer
is "ask the install", not a list frozen at packaging time.

**A resolver domain has no fallback.** A resolver that throws or times out is an
error that names it, not a quiet degradation to a stale list: a fallback would
paper over the actual failure and hand the user a backend the build may not even
have, which is worse than refusing to launch with a message. The one thing a
fallback bought — showing *something* in a settings UI when the emulator is not
installed — the UI can express directly as an unavailable option.

Defaults follow the domain: a literal, a `strategy` over the resolved domain
(`first_where`), `{ source = "resolver:…" }` for one the module computes, or
omitted entirely — in which case the first resolved choice wins. Resolvers return
an ordered list and that order is meaningful, so "the first one" is usually the
answer the author already encoded. Lowering checks that every named export exists
in the manifest's module, so a typo fails at load rather than resolving to
nothing at launch.

```rust
pub struct Choice {
    pub value: Value,                 // stored + emitted
    pub label: Arc<str>,              // display only, never persisted
    pub description: Option<Arc<str>>,
    pub availability: Availability,
    pub meta: Option<Arc<Map<Arc<str>, Value>>>,  // unprojected host record fields
}

pub enum Availability {
    Available,
    Unavailable { reason: Arc<str> },  // greyed out, e.g. "bios/scph39001.bin not found"
}
```

`meta` is what `first_where` evaluates against, reusing the same closed `Expr`.
**`value` must be stable across host state changes**; lowering warns when
`value_field` names something ordinal, because a driver reshuffle would otherwise
silently repoint saved config.

### 3.6 Expressions

Closed and total. No calls, no arithmetic, no user extension — which is what lets
the same `Expr` lower to a JS predicate for the settings UI, so enable/disable
evaluates client-side from one source of truth.

```rust
pub enum Expr {
    Lit(Value), Ref(OptionId),
    Cmp { op: CmpOp, lhs: Box<Expr>, rhs: Box<Expr> },
    In  { needle: Box<Expr>, set: Arc<[Value]> },
    And(Vec<Expr>), Or(Vec<Expr>), Not(Box<Expr>),
    HostHas(CapabilityUri),
}
```

Anything it cannot express escapes to a TS predicate.

---

## 4. IR and lowering

```rust
pub struct Definition {
    pub meta: Meta,
    pub targets: TargetGraph,
    pub sections: Vec<Section>,           // flat arena; tree survives as `parent`
    pub options: Vec<OptionDef>,          // flat arena
    pub by_path: HashMap<OptionPath, OptionId>,
    pub order: Vec<OptionId>,             // precomputed resolution order
    pub env: Vec<EnvDef>,                 // name + literal-or-option source
    pub argv: Vec<ArgvDef>,               // ordered, each with an optional `when`
    pub bool_styles: Vec<BoolStyle>,      // interned; options hold a BoolStyleId
    pub input: Option<InputDef>,
    pub actions: Option<ActionDef>,
    pub hash: ContentHash,                // blake3 over the canonicalized document
}

pub struct OptionDef {
    pub id: OptionId,
    pub path: OptionPath,                 // "video.adapter" — stable storage key
    pub section: SectionId,
    pub ty: ValueType,
    pub domain: Domain,
    pub default: DefaultSpec,
    pub emit: Option<EmitSpec>,           // None ⇒ UI-only
    pub enabled_when: Option<Expr>,
    pub when_disabled: DisabledPolicy,    // omit | default | retain
    pub deps: SmallVec<[OptionId; 2]>,
}

pub struct Section {
    pub id: SectionId,
    pub path: SectionPath,                // "video.advanced" — the option prefix
    pub parent: Option<SectionId>,
    pub title: Arc<str>,
    pub target: Option<TargetId>,         // inherited default for member options
    pub children: SmallVec<[SectionId; 4]>,   // document order
}

// Target inheritance is resolved during lowering, so nothing downstream walks
// ancestors to find out where a value goes.
pub struct EmitSpec { pub target: TargetId, pub key: Arc<str>, pub repr: Repr }

pub enum Repr {
    Bool(BoolStyleId),                    // index into Definition::bool_styles (§3.4)
    Int { radix: Radix, width: Option<usize> },
    Str { quote: QuoteStyle },
    List { sep: Arc<str> },
    Mapped(Arc<HashMap<Value, Arc<str>>>), // "Vulkan" -> "3"; also numeric keycodes
    // Cemu writes a hotkey as `"<wx keycode> <pad index>"` — two values in one
    // node — so a single key is not always a single value.
    Composite { parts: Arc<[Repr]>, sep: Arc<str> },
}
```

`when_disabled` is per-option, not global: emulators differ on how they treat a
missing key, and it is painful to retrofit once plugins exist.

### 4.1 Lowering rejects

Static structure:
- unknown target reference; cycle in the target graph or in `deps`
- a target declaring both a nesting position and `parent`
- a root target (one with `format`/`path`) that is nested, or a nested target
  that declares `format`/`path`
- a child table whose key collides with a target field name (`format`, `path`,
  `node`, `parent`, `overrides`) or a section field name (`title`, `target`,
  `options`, `repeat`, `description`)
- an option with an `emit` whose target is neither stated nor inherited from any
  ancestor section
- `enabled_when` referencing an undeclared path; type mismatch between operands
- default outside the declared range, or absent from a static domain
- a static domain with no default, or a `strategy` default on a domain that
  cannot satisfy it
- a `resolver:` domain or default naming an export the manifest's module does not
  have, or naming one at all when the manifest declares no resolvers
- `Repr::Mapped` missing an arm for a reachable choice
- a `bool_style` reference naming an undeclared style, or a declared style
  missing either member
- two options emitting the same `(target, key)` without an explicit `overrides` marker

Environment and arguments:
- two root targets writing the same file path without `overrides`
- an `[env]` name matching a host-owned variable or reserved prefix (§8.3)
- an `[env]` or `[[argv]]` entry referencing an undeclared option path
- an `[[argv]]` entry with a `flag` but no `value`

Input (§6):
- a binding key that is not an element of the layout it is declared under —
  the layout's elements, not a global list, since consolespec expands them
  per device (§6.5.1)
- a port mode of `kind = "managed"` without a `layout`, or with a layout that
  is not an inputspec id
- a port group that the machinespec does not declare, or a layout the group
  does not accept (§6.5)
- a partial atom table for a vector element kind
- `[input]` present with `distinct_devices = false` and more than one managed port

Actions (§7):
- an action name outside the global vocabulary
- a `key` absent from the platform virtual-key table
- a non-`fixed` action with no emulator-side `name`
- `transport` inconsistent with the declared `keyboard_path`

---

## 5. Values

A resolution stack, folded at realize time. **This crate defines the types and the
fold; it never loads or saves a layer.**

```
[ definition defaults, user profile, per-game, runtime overrides, system-forced ]
```

Each layer is a sparse `OptionPath → Value` map. Forced values (background input
required by managed actions, a hook-compatible backend) sit above the user's and
retain provenance, so the UI can say *"forced by system"* rather than appearing to
ignore the user.

```rust
pub struct ValueStack { pub layers: Vec<(LayerId, BTreeMap<OptionPath, Value>)> }

pub enum Resolution {
    Matched(Choice),
    Orphaned { stored: Value, effective: Value },
}

pub struct ResolvedValues {
    pub definition: ContentHash,
    pub entries: BTreeMap<OptionPath, Resolved>,   // value + Resolution + winning LayerId
}
```

Two invariants the caller must honor when it persists:

1. **Key against `Definition::hash`, never the realized form.** Otherwise saved
   config depends on which GPU was plugged in when it was saved.
2. **Persist orphans as stored, not effective.** Unplug the configured GPU, launch
   on the iGPU, plug it back in — the setting returns.

---

## 6. Host facts

Everything realize needs that is not in the document or the values. The host
supplies it; the crate never discovers it.

```rust
pub struct HostFacts<'a> {
    pub ports: &'a [PortFact],             // resolved topology for this launch
    pub emulator: EmulatorBuild,           // version + sdl_major, for applies_to
    pub capabilities: &'a dyn CapabilityRegistry,   // gpu.adapters, display.modes, …
    pub platform: Platform,                // key table selection
}

pub struct PortFact {
    pub port: u8,                          // emulator-facing port index
    pub sdl_index: Option<u8>,             // dense index among managed ports — see §6.3
    pub mode: PortModeId,
    pub layout: Option<InputLayoutId>,
    pub sdl: Option<SdlOutputLayout>,      // from InputLayout::sdl_output()
}
```

`EmulatorBuild::sdl_major` matters twice: PCSX2's bind syntax differs between SDL2
and SDL3 builds of the same emulator, and snowglass's own SDL2 path has no sensor
support (§6.2). The host detects it from the target's import table; a plugin may
pin it.

**consolespec is the only input vocabulary.** Plugin documents name inputspec
layouts, consolespec binding keys and machinespec input groups, and so does the
runtime. The `consolespec` crate parses and cross-checks the documents in its
build script and exposes the result as compile-time tables;
`InputLayout::all()` (`snowglass-input/src/layout.rs`) expands them once into the
controls of §6.5.1. There is no registry to pass, no translation step and no
second layout set — the ids in a plugin document are the ids the hub, the SDL
hooks, and `SdlOutputLayout` use. A layout id that consolespec does not declare
fails to resolve rather than binding to nothing, and a machine id likewise
(§6.5).

Because the data is compiled in, there is no directory to point at: a newer
consolespec reaches a binary by rebuilding it.

### 6.1 Input is not a static document — it is parameterized by layout

**[V]** The SDL3 hook builds each virtual pad from the port's layout:
`InputLayout::sdl_output()` (`snowglass-input/src/layout.rs`) folds the port's
controls through the shared projection, and the hook maps semantic index → device
index through `dense_mask_index` (`snowglass-hook-sdl3/src/api.rs:46`). A
GameCube layout and a Switch Pro layout therefore produce **different raw
joystick indices for the same control**.

Consequences, which replace §7.3 and §10 of the previous design:

- **Raw-index binding is unsound.** Mapped presentation is not the safer of two
  options; it is the only correct one.
- **There is no `profiles/sdl.gamepad.toml`.** The control set is
  `InputLayout::all()` + `sdl_output()` at realize time — compile-time consolespec
  data, expanded once. Nothing is generated from a distribution file, and there is
  no version field to drift.
- **Fragments are filtered per port.** A binding for a control absent from that
  port's layout is silently suppressed, not emitted as a dead key. Lowering warns
  only if a control appears in no layout reachable from any declared port mode.
- **Reachability is not the plugin's business.** Whether SDL can carry a control
  is the projection's answer and changes with it, so a generated plugin document
  states every binding its emulator has and lets the runtime drop what cannot
  fire. Baking the answer in would mean regenerating every plugin whenever the
  projection moved a control.

**[V]** Identity *is* stable: the name is the constant `"Snowglass Virtual
Gamepad"` (`snowglass-hook-sdl3/src/install.rs:92`,
`snowglass-hook-sdl2/src/api.rs:55`), vendor/product are `0/0`, and
`GamepadButton` is a fixed 0–20 enum (`snowglass-input/src/gamepad.rs`) pinned to
`sdl::BUTTONS` by `wire_order_matches_the_pad`. So the SDL GUID is stable — which
is what makes pinning a mapping possible at all.

### 6.2 Two prerequisites in snowglass proper

These are not this crate's work, but the input design is unsound without them.

- **P1 — SDL2 must honor `SdlOutputLayout`.** **[V]** `snowglass-hook-sdl2` hard
  codes `button_mask = (1 << COUNT) - 1` and a single name
  (`src/api.rs:50‑55`) and `Api::load` takes no layouts, while SDL3 takes
  `Vec<(SdlOutputLayout, String)>`. The two backends present *different topologies
  today*. The SDL2 `VirtualJoystickDesc` already has the mask fields; it needs the
  same plumbing SDL3 has in `install.rs:88‑96`. Until then, an SDL2 emulator sees
  every element regardless of layout.
- **P2 — the virtual pad name must include the layout id.** All ports currently
  share one name, hence one GUID, hence one `SDL_GAMECONTROLLERCONFIG` mapping —
  which cannot be correct for two ports with different masks. Making the name
  `"Snowglass Virtual Gamepad (GAMECUBE_CONTROLLER)"` yields a distinct GUID per
  topology and lets §6.4 emit one mapping per distinct layout. It also gives
  emulators that identify devices by name a way to tell ports apart.

Sensors are SDL3-only: the SDL2 virtual joystick descriptor has no sensor array at
all. `sensor_*` fragments are suppressed when `sdl_major < 3` — this replaces the
`min_sdl = "2.0.14"` version string, which cannot express "this backend never
supports it".

### 6.3 Port index skew

`EmulatorConfig::gamepad_layouts` is positional: index = virtual pad slot = SDL
enumeration order = `{index}` in a device string. Passthrough and empty ports
consume an emulator port but **no** virtual pad, so port 3 with port 2 in
passthrough has `sdl_index = 2`. `PortFact` carries both, target expansion uses
`{n}` = `port`, and device templates use `{index}` = `sdl_index`. Getting this
wrong binds player 3's config to player 2's pad, silently.

### 6.4 Emitted bindings

**The definition document is complete.** Everything needed to write an emulator's
input configuration is either in that emulator's own definition or in snowglass's
shared tables. There is no second document beside it and no per-emulator dialect
to select: a plugin states its emulator's vocabulary, and snowglass states where
each control lives.

**An input port is an ordinary section.** It is not a separate `input.toml`
grammar — it is a section like any other, in the emulator's configuration path,
carrying `input = true`:

```toml
[sections.gcpad.pad1]
title   = "GameCube Controller 1"
target  = "gcpad.GCPad1"
input   = true                        # see below
layouts = ["GAMECUBE_CONTROLLER"]
```

`input = true` is what turns a section into a port. It means three things and
nothing else:

1. **`{index}` and `{name}` expand** in the values this section emits. `{index}`
   is the virtual pad slot — §6.3, *not* the port number — and `{name}` the pad's
   name for that port's layout, so P2's rename propagates without touching a
   plugin.
2. The section's emissions are **filtered by the port's selected layout**, so a
   control the chosen device does not have is never written.
3. The section gains the implicit `layout` option of §6.5.2, whose domain is
   `layouts` plus `unplugged` and, where declared, `passthrough`.

Everything else about it is a section: `target` says where it writes, `emit`
entries and options behave exactly as elsewhere, and it nests in the same tree.
That is the point — an input port was never a different kind of thing, and giving
it its own document made it look like one.

**What a plugin still declares per layout** is the emulator's own configuration
key for each control (§6.5.1). The left side is a consolespec binding key, the
right is the emulator's vocabulary and is the plugin's business alone:

```toml
[input.layouts.GAMECUBE_CONTROLLER]
"button.a"             = "Buttons/A"
"button.z"             = "Buttons/Z"
"analog.stick.left.x+" = "Main Stick/Right"
```

**What snowglass supplies is where the control *is*.** Which SDL control carries
a device control is the projection's answer — `snowglass-input`'s
`projection/SDL.proj.toml` — and it is shared, so "which physical button is C-Up"
is the same in every emulator. Both sides of it are binding keys: the left of the
device, the right of the SDL virtual gamepad, which `snowglass-input::sdl`
describes as a device in the same vocabulary. `sdl::wire` then places an SDL
control at its `SDL_GAMEPAD_BUTTON_*` position.

**The emulator's configuration is static.** Once a port's layout is chosen the
whole of it is determined, written once, and never touched again:

```
"button.z"  --plugin-->     "Buttons/Z"        the emulator's config key
"button.z"  --projection--> the pad's button.r
            --sdl::wire-->  button 10
            --plugin-->     "Shoulder R"       the emulator's name for that control
```

`GCPad1: Buttons/Z = ``SDL/0/Snowglass Virtual Gamepad:Shoulder R``` — and that
line is the same for every user, every session, and every remapping.

**Dolphin never sees an override.** All user remapping is absorbed on the virtual
gamepad side: an override changes what the pad *reports* for a control (§7), not
what the emulator's file says. That is the whole reason the file can be static,
and it is why there is no live config rewriting, no per-user fragment, and no
reason for an emulator to reload its input configuration mid-session.

**Both halves of a binding come from the plugin's own emulator.** The key side is
that emulator's control name (`Buttons/Z`); the value side is that emulator's
name for an *SDL* control — and those are a fixed vocabulary of 21 buttons and 6
axes, indexed by SDL's own position, which every generator can extract from its
emulator's source the way it extracts everything else. Dolphin's live in
`InputCommon/ControllerInterface/SDL/SDLGamepad.h`:

```cpp
static constexpr std::array<const char*, 21> s_sdl_button_names = {
    "Button S",  // SDL_GAMEPAD_BUTTON_SOUTH
    ...
    "Shoulder R",  // SDL_GAMEPAD_BUTTON_RIGHT_SHOULDER
    "Pad N",       // SDL_GAMEPAD_BUTTON_DPAD_UP
```

So the value side is **not** a grammar over indices — Dolphin spells button 10
`Shoulder R`, not `Button 10` — and it is **not** per layout. It is one table per
emulator, over a vocabulary SDL fixes, and it belongs in that emulator's own
definition:

```toml
[input.sdl]                        # not yet emitted; extracted like any other table
buttons = ["Button S", "Button E", …, "Shoulder R", "Pad N", …]
axes    = ["Left X", "Left Y", "Right X", "Right Y", "Trigger L", "Trigger R"]
```

With that, realize composes the value from three things it already has — the
plugin's key table, the shared projection, and the plugin's SDL names — and
nothing lives outside the document.

Static bindings are **not options** — no user ever sets `Buttons/A`. They lower to
`Node::Entry` with `origin.layer = Fragment(id)` and are always snowglass-owned on
merge.

**Units and frame stay out of the config layer.** Snowglass presents SDL
convention (rad/s, m/s², right-handed) from every physical source; per-emulator
sensitivity and deadzone are ordinary options. `AxisTransform`
(`snowglass-input/src/hub.rs`) already owns invert/scale/deadzone on the snowglass
side. A config file names a binding; it cannot transform a value.

### 6.5 Ports are declared per platform

`InputHub` is already "the control plane for input" — revisioned, transactional,
with `PortConfiguration { port, layout, source, overrides }`
(`snowglass-input/src/hub.rs:130`) and an `InputOperation::ReplacePorts` whose doc
comment says hosts use it "when attaching a game whose declared controller ports
differ from the previous game" (`:151`). **The definition supplies topology; the
hub remains the authority.** This crate does not model bindings, sources, or
overrides — only which ports exist and what each one accepts.

Which ports exist is a property of the *machine being launched*, not of the
emulator. Dolphin running a GameCube title has four SI ports; running a Wii title
it has four Wii Remote slots **and** four SI ports, because Wii games read GameCube
controllers too. So ports hang off the machine.

They also hang off an **input group**. Those two families of Wii port are not
interchangeable — different bus, different devices — and a flat numbering can
only distinguish them by an interleaving convention that nothing records. The
machinespec already names the groups, so ports are keyed by group and then by
index within it:

```toml
# consolespec/machinespec/NINTENDO_WII.toml
[input]
groups = ["wii", "gamecube", "usb"]

[input.wii]
inputs = ["WII_REMOTE_CONTROLLER", "WII_REMOTE_NUNCHUCK_CONTROLLER"]
ports  = 4
```

A group's `inputs` is the authority on what may be plugged in there, and
lowering checks the plugin's `layouts` against it. A group's `ports` is the
machine's base port count; a plugin may declare **fewer** (an emulator need not
implement every port) and may declare **more** only where a multitap accounts
for the difference — DuckStation exposes eight pad slots on a two-port
PlayStation. Exceeding the count is a warning, never silent.

A port declaration is the section of §6.4 — `input = true` is what makes it one
— and the `[platforms.…]` spelling below is how the generators emit it today.
Reconciling the two is the open work of §6.4; nothing else about a port changes.

```toml
[platforms.NINTENDO_GCN.ports.controller.0]
target  = "gcpad.GCPad1"
layouts = ["GAMECUBE_CONTROLLER"]
emit.unplugged           = [{ target = "main.Core", key = "SIDevice0", value = "0" }]
emit.GAMECUBE_CONTROLLER = [{ target = "main.Core", key = "SIDevice0", value = "6" }]

[platforms.NINTENDO_WII.ports.wii.0]
target      = "wiimote.Wiimote1"
layouts     = ["WII_REMOTE_CONTROLLER", "WII_REMOTE_NUNCHUCK_CONTROLLER"]
passthrough = { transport = "bluetooth-hci", requires = ["host:bluetooth.adapter.exclusive"] }
emit.unplugged   = [{ key = "Source", value = "0" }]
emit.passthrough = [{ key = "Source", value = "2" }]
emit.WII_REMOTE_CONTROLLER = [
  { key = "Source",    value = "1" },
  { key = "Extension", value = "None" },
]
emit.WII_REMOTE_NUNCHUCK_CONTROLLER = [
  { key = "Source",    value = "1" },
  { key = "Extension", value = "Nunchuk" },
]
```

A port's choices are exactly: the inputspec layouts in `layouts`, `unplugged` always, and
`passthrough` when the port declares it. There is no separate `kind` field —
naming a layout *is* choosing a managed port, and the emissions under that key
are what "this port is now that" means in the emulator's config. `emit` entries
without a `target` land in the port's own; the GameCube device-type key lives in
a different file, so it names one.

**Every port names its own target and its own keys, so no template arithmetic is
needed anywhere.** Dolphin numbers pad sections from 1 and SI devices from 0; a
`{n}`/`{n-1}` scheme would have to encode that, and every emulator's variant of
it. Twelve explicit port declarations are longer and cannot be wrong.

The platform keys must be machinespec ids that the manifest's `platforms` list
also declares, and lowering checks both.

### 6.5.1 Layout bindings

What a control is *called* is a property of the layout, not of the port, so key
tables are declared once and referenced by every port that offers the layout:

```toml
[input.layouts.WII_REMOTE_CONTROLLER]
"button.a"                 = "Buttons/A"
"button.select"            = "Buttons/-"
"button.guide"             = "Buttons/Home"
"directional.n"            = "D-Pad/Up"
"pointer.x+"               = "IR/Right"
"analog.gyroscope.y+"      = "IMUGyroscope/Pitch Up"
"analog.accelerometer.z+"  = "IMUAccelerometer/Up"

[input.layouts.WII_REMOTE_NUNCHUCK_CONTROLLER]
"button.a"                        = "Buttons/A"
"button.c"                        = "Nunchuk/Buttons/C"
"analog.stick.left.y+"            = "Nunchuk/Stick/Up"
"analog.accelerometer.right.z+"   = "IMUAccelerometer/Up"
"analog.accelerometer.left.z+"    = "Nunchuk/IMUAccelerometer/Up"
# ... and the rest of the remote's controls, restated
```

**A layout table declares its bindings in full; there is no inheritance.** The
two tables above overlap heavily and it is tempting to let the second extend the
first — consolespec models a Wii Remote and a Wii Remote with a nunchuk as two
layouts, while Dolphin models one controller with a swappable extension, so the
second really is the first plus five controls. It is not safe. A key is stable
against its *own* cluster's fields, not against the device's: attaching the
nunchuk gives the device a second accelerometer, which forces the remote's own
to declare `alignment = "right"`, and `analog.accelerometer.z+` becomes
`analog.accelerometer.right.z+`. An inherited table would carry six keys the
child device does not have — which is exactly what the `extends` key did before
it was removed, undetected, because nothing revalidated an inherited key against
the child. Restating is longer and cannot be wrong.

Keys are validated against the layout's own expanded key set at lowering — a
table may not name a control the device does not have — and against the
emulator's own vocabulary where a generator can extract it (§10). That first
check is not cosmetic: it is what caught the Cemu plugin binding its Wii Remote
through the Pro Controller's table, sixteen controls a Wii Remote does not have.

The key set is *derived*, because consolespec declares clusters rather than
controls. A key is built from the declaring cluster's own fields and nothing
else, so a device gaining a cluster never renames a key belonging to another.
The grammar:

```
button.<element>
directional[.<alignment>].<direction>
analog.<class>[.<alignment>].<axis><sign>
trigger.<alignment>
rumble.<alignment|size>
pointer[.<alignment>][.<axis><sign>]
touchscreen|keyboard|microphone|camera [.<alignment>]
```

The expansion, in full:

| inputspec entry | binding keys |
|---|---|
| `button` with `element = "a"` | `button.a`; `start`/`select`/`guide`/`clickl`/`clickr` likewise, and `"0"`-`"31"` give `button.0`-`button.31` |
| `directional` with `directions = 4` | `directional.n`, `.e`, `.s`, `.w` (`directions = 8` adds the four intercardinals) |
| `analog` with `axes = 2` | `analog.stick.{x,y}{+,-}` — one positive and one negative per axis, over x, y, z as `axes` allows. `class` defaults to `stick` |
| `analog` with `class = "gyroscope"`/`"accelerometer"`/`"slider"`/`"rotary"` | `analog.gyroscope.…`, `analog.accelerometer.…`, same expansion |
| `alignment` on any of the above | inserted before the component: `analog.stick.left.x+`, `directional.right.n` |
| `trigger` with `alignment = "left"` | `trigger.left` |
| `rumble` with `size = "big"` | `rumble.big`; a motor with an `alignment` uses that instead |
| `pointer` with `dimensions = 2` | `pointer` for the absolute position, plus `pointer.{x,y}{+,-}` |
| `touchscreen`, `keyboard`, `microphone`, `camera` | `touchscreen`, `keyboard`, `microphone`, `camera` |

`alignment` appears only where a device has two clusters of one kind and class
to tell apart. A key that omits it means the device's *primary* cluster of that
kind, which is how one keyboard-defaults document serves every layout:
`directional.n` reaches the N64's `directional.left.n` and no other. Only the
first cluster, in declaration order, answers to the unqualified key.

The dotted string is the *wire* form only. In the runtime a key is
`snowglass_input::key::BindingKey`, a `Copy` sum type parsed at document load,
so a malformed key fails there — naming itself — rather than being dropped
silently where nobody looks, and no consumer picks a key apart with `starts_with`.

Controls also carry an *id* (`ButtonA`, `AxisLeftAnalogPositiveX`) for display,
and where two clusters expand onto one id — the N64's two directionals, the
DualSense's two small motors — the second takes an ordinal suffix
(`DirectionalN2`) in declaration order. Ids are not binding keys and nothing
persists them.

`options` is reserved, so a layout table may not bind a control by that name.

### 6.5.2 Layout options

A layout also carries the emulator's own settings for that device — Dolphin's
sideways and upright Wii Remote, its battery level, whether a MotionPlus is
attached. These are ordinary options in every respect except that they exist
*per port*, and only while that port holds this layout:

```toml
[[input.layouts.WII_REMOTE_CONTROLLER.options]]
key     = "sideways"
title   = "Sideways Wii Remote"
type    = "bool"
emit    = { key = "Options/Sideways Wiimote", bool_style = "upper" }
default = false

[[input.layouts.WII_REMOTE_CONTROLLER.options]]
key     = "battery"
title   = "Battery"
type    = "int"
emit    = { key = "Options/Battery" }
range   = { min = 0, max = 100 }
default = 95
```

`range` is one value, not two loose keys: a bound without its partner is almost
always a mistake, and lowering checks `default` against it as a unit (§4.1).

Lowering expands each one across every port that offers the layout, producing an
option at `ports.<platform>.<index>.<key>` whose `emit.target` is that port's
target. So a four-remote Wii setup gets four independent `sideways` settings, and
their values persist per port rather than bleeding across them.

Like the key table (§6.5.1), a layout declares its options in full — a nunchuk
port keeps the remote's options because `WII_REMOTE_NUNCHUCK_CONTROLLER` states
them again, not because it inherits them.

**Every port has an implicit option, `ports.<platform>.<index>.layout`**, whose
domain is the port's `layouts` plus `unplugged` and, where declared,
`passthrough`. It is not written in the document — the port already lists its
choices, and repeating them could only disagree. Two things hang off it: the
`emit.<choice>` fragments (§6.5) are selected by its value, and each layout
option's `enabled_when` is generated against it, so a remote's `sideways` greys
out the moment that port becomes a GameCube pad. That predicate is an ordinary
`Cmp` over an `OptionId`, so the settings UI evaluates it client-side from the
same lowered `Expr` as everything else (§3.6).

**Passthrough** is the only surviving dynamic enumeration: raw HID/HCI (motion,
IR, extension hotplug, speaker, rumble) does not survive projection onto SDL. It
contributes one device-selection option per port that declares it.

**Emulators that do not use SDL for controllers are out of scope.** They omit
`[input]`, snowglass writes no input config, and the user configures in-emulator.

### 6.6 Pointer

**[V]** The previous design's open risk is smaller than assumed. `PointerInput`
carries an absolute position in presentation coordinates
(`snowglass-input/src/mouse/mod.rs:59`) and there are three delivery paths:

| transport | reaches | evidence |
| --- | --- | --- |
| `sdl-axis` | emulators reading raw joystick axes 6/7 | `layout.rs`, `sdl_output()` — always present, outside `SDL_GAMEPAD_AXIS_COUNT` |
| `win32-message` | Qt/wx/SDL-windowed emulators | `snowglass-hook-win32/src/lib.rs:1` |
| `dinput-mouse` | Dolphin's `Keyboard Mouse` device on Windows | `snowglass-hook-dinput/src/runtime.rs:243` |

So it is a transport selection, not a capability flag that is mostly false:

```toml
[input.pointer]
transport = "dinput-mouse"
target    = "controls-{n}"
[input.pointer.keys]
x = "IR/X"
y = "IR/Y"
```

Gamepad-only emulators (PPSSPP) cannot see `sdl-axis`, because axes 6/7 are raw
joystick axes that no gamepad mapping covers. That is a lowering-time check
against the mode, not a runtime surprise.

### 6.7 Cross-cutting rules live in the TS module

Two rules cut across options and cannot be written as an option's own
`enabled_when`: the overlay attaches only to renderers snowglass hooks, and no
two passthrough ports may claim the same physical device. Neither is expressible
in the closed `Expr` (§3.6), and neither is a fact about the emulator's config
file — both are statements about snowglass. So the definition document declares
no constraint mechanism at all; the TS module enforces them where it already has
the information.

For the renderer coupling that is not a loss but a tightening, because the table
that maps a backend onto hooks *is* the validation:

```ts
hooks: HOOKS[ctx.get("main.core.gfx_backend")] ?? fail("…requires Vulkan or D3D11"),
```

One object, so a mapping and its guard cannot drift apart — which two
declarations, one in TOML and one in TS, eventually would.

Device exclusivity needs no expression either: two ports claiming one device
produce two identical `ResourceClaim`s, and the host fails the acquisition
before launch with the device named (§8.2). A rule enforced where the resource
is actually taken cannot be bypassed by a descriptor that skipped the check.

What the config layer keeps is the per-option half: `enabled_when` still greys
out an option, `Availability::Unavailable` still explains why a choice is
unselectable, and realize still returns `Diagnostics`.

---

## 7. Actions

### 7.1 Model

Actions are never exposed to the user. The shell receives `save_state`, dispatches
over IPC, and the hooks synthesize keyboard input so it behaves as if it came from
a real keyboard. Keyboard only: the gamepad pool is small, and PPSSPP corrupts
unmapped devices.

Because the shell owns focus, the emulator receives *only* synthesized keys.
"Unused key" is meaningless, so there is no pool to allocate — **map actions to
each emulator's conventional keys**, so the emitted config matches that emulator's
own documentation.

The action vocabulary is a fixed global set (save/load state, screenshot, pause,
frame advance, fast forward, reset, exit, toggle fullscreen); plugins declare a
subset.

### 7.2 Delivery is not uniform

The previous design dropped the `delivery` field. **[V]** The code already has two
transports and the caller picks between them: `Message::RawKeyInput` posts
`WM_KEYDOWN`/`WM_KEYUP`, and `RawKeyInput::direct_input_data()`
(`snowglass-input/src/lib.rs:66`) projects the same transition into a buffered
DirectInput record — its own doc comment says *"Callers choose the delivery
backend."* The survey is the reason why: Dolphin reads DInput, DuckStation and
melonDS read Qt (window messages), PPSSPP reads SDL events.

So `transport` returns — not as a user knob, but as the fact the survey already
collected:

```toml
[actions]
target        = "hotkeys"
template      = "Keyboard/{key}"
keynames      = "duckstation"
transport     = "win32-message"     # win32-message | dinput
applies_to    = ">=0.1-7000"

[actions.map]
load_state        = { key = "F1",     name = "LoadSelectedSaveState" }
save_state        = { key = "F2",     name = "SaveSelectedSaveState" }
fast_forward      = { key = "Tab",    name = "FastForward" }
pause             = { key = "Space",  name = "TogglePause" }
screenshot        = { key = "F10",    name = "Screenshot" }
toggle_fullscreen = { key = "F11",    name = "ToggleFullscreen" }
exit              = { key = "alt+return", fixed = true }   # hardcoded; emits nothing
```

`fixed = true` means the emulator hardcodes the binding: send the key, write
nothing. Distinct from a missing `name`, and worth re-verifying on version bumps.

### 7.3 Two key tables, both required

- **Platform table** (this crate, `action/vk.rs`): `"F1"` → `HotkeyKey { virtual_key,
  scan_code, extended }`, matching `snowglass-runtime/src/instance.rs:254`. The
  descriptor carries resolved chords so the runtime parses no key names.
- **Emulator table** (plugin, `Repr::Mapped`): `"F1"` → whatever that emulator
  writes — `Keyboard/F3`, `input_*_key = f3`, an `NKCODE_*` name, or a raw Qt
  integer.

Four distinct encodings across the survey; the table is shareable only within the
PCSX2/DuckStation pair (shared stenzek `InputManager` — **[?]** confirm).

### 7.4 Survey

| emulator | keyboard path | transport | remappable | location | encoding |
| --- | --- | --- | --- | --- | --- |
| Dolphin | DInput / Xlib / Quartz **[V]** | `dinput` | yes | `Hotkeys.ini`, profile-scoped | expression string |
| PCSX2 | InputManager | `win32-message` | yes | `[Hotkeys]` in `PCSX2.ini` | `Keyboard/F3` |
| DuckStation | Qt **[V]** | `win32-message` | yes | `[Hotkeys]` in `settings.ini` | `Keyboard/F3` |
| RetroArch | own drivers | `win32-message` | yes | `retroarch.cfg` | `input_*_key = f3` |
| PPSSPP | SDL events **[V]** | `win32-message` | yes | PPSSPP ini | NKCODE table |
| melonDS | Qt **[V]** | `win32-message` | yes | `melonDS.ini` | numeric Qt keycode |
| Cemu | wx **[V]** | `win32-message` | yes **[V]** | `<Hotkeys>` in `settings.xml` | wx keycode + pad index, space-separated |

- **PPSSPP** drops any key absent from `KeyMapRawSDLtoNative` (`break`, no error).
  `NKCODE_*` is Android-derived; **[?]** F13+ probably does not exist.
- **melonDS** stores raw `int` Qt keycodes (`QKeySequence(Qt::Key_O | Qt::CTRL |
  Qt::SHIFT)`, cf. issue #1106). `Repr::Mapped` with `encoding = "int"`.
- **Cemu** no longer hardcodes its shortcuts — issue #721 is stale. `wxCemuConfig`
  reads seven from `<Hotkeys>`, four with defaults. It has no save-state, load-state,
  pause, frame-advance or reset hotkey at all, so five of the nine vocabulary
  actions have no Cemu equivalent and must be *hidden* rather than left unbound.
- **DuckStation** **[V]** defaults live in `src/core/hotkeys.cpp`, partly inside a
  platform `#ifdef` — the available action set may differ per build.

### 7.5 Version drift is bidirectional

Observed in DuckStation: an older generated config reads `OpenQuickMenu =
Keyboard/Esc`; current source has `OpenPauseMenu = Keyboard/Escape`. Both the
action name and the key name changed, and both fail silently — unknown section
keys are ignored, unknown key names fail to parse. `applies_to` versions the whole
`[actions]` block against `HostFacts::emulator`.

### 7.6 Stuck keys

If an emulator flushes its event queue during a state load or renderer reset, it
can see a key-down and never the key-up. The shell asserts "no action keys held"
every N frames. Cheap; prevents a miserable class of bug. Not this crate's code,
but the descriptor carries the held-key set so the shell knows what to assert on.

---

## 8. Realize, emit, and the descriptor

```rust
pub fn realize(
    doc: &Definition,
    layers: &ValueStack,
    facts: &HostFacts<'_>,
    resolvers: &dyn ResolverRuntime,
) -> Result<(RealizedDefinition, ResolvedValues, Diagnostics)>;

pub fn emit(
    realized: &RealizedDefinition,
    values: &ResolvedValues,
    existing: &dyn ExistingFiles,     // borrowed bytes, host-supplied; no I/O here
) -> Result<Realized>;
```

`realize` iterates `doc.order` — a toposort over `deps` and `enabled_when` refs, so
a cycle fails loudly at load rather than producing different files on different
runs. Per option: resolve domain, evaluate enable, fold layers, commit.

### 8.1 Emission AST

```rust
pub enum Node {
    File    { path: PathBuf, format: FormatId, children: Vec<Node>, ann: Annotations },
    Section { name: Arc<str>, children: Vec<Node>, ann: Annotations },
    Entry   { key: Arc<str>, value: EmittedValue, origin: Origin, ann: Annotations },
    Raw     { bytes: Vec<u8> },      // preserved unknown / hand-edited content
    Comment { text: Arc<str> },
}

pub struct Origin { pub option: Option<OptionId>, pub layer: LayerId }
// LayerId: Default | Profile | Game | Runtime | Forced | Fragment(&'static str) | Foreign

pub trait Rewriter { fn rewrite(&mut self, node: Node) -> Option<Node>; }
pub trait Codec {
    fn parse(&self, bytes: &[u8]) -> Result<Node>;
    fn emit(&self, node: &Node) -> Result<Vec<u8>>;
}
```

Visitors are folds, not `&mut` walks — `Remove`/`Replace` against a borrowed tree
gets unpleasant fast.

Writing config is a **merge**, not a clobber:

```
parse(existing) ──┐
                  ├─► merge(policy) ─► rewriters ─► Codec::emit
realize(def) ─────┘
```

Foreign keys and comments survive. `Origin::Fragment` and `Origin::Forced` are
always snowglass-owned. Host formats: `ini`, `cfg`, `xml`, `json` — ~95% of
emulator config. Plugins export `emit` only for genuine oddities.

There is no `cmdline` codec. Arguments are rendered directly from `[[argv]]`:
there is nothing to parse, nothing to merge, and no foreign content to preserve,
so the round-trip machinery would be dead weight.

### 8.2 Realize output

```rust
pub struct Realized {
    pub config:  BTreeMap<String, String>,  // config-root-relative path → file contents
    pub env:     BTreeMap<String, String>,
    pub argv:    Vec<String>,
    pub ports:   Vec<PortPlan>,        // layout per port, in port order
    pub actions: Vec<ActionBinding>,   // resolved chord + transport
    pub pointer: Option<PointerPlan>,
    pub claims:  Vec<ResourceClaim>,   // acquire before launch, release on teardown
    pub diagnostics: Diagnostics,
}

pub enum ResourceClaim {
    ExclusiveDevice  { id: DeviceId, transport: Transport },
    BluetoothAdapter { id: AdapterId },
    SnowglassProfile { port: u8, profile: ProfileId },
}
```

`emit` **returns** contents rather than writing them: sandboxing, unit tests with
no filesystem, and a free "preview generated config" feature. Unsatisfiable claims
fail *before* launch with a specific message, rather than after launch with a
silently dead controller.

`config` is a flat `path → contents` map, not a list of structs, because that is
what it becomes on the wire (§8.4). Two consequences worth stating:

- **Contents are `String`, so config output is UTF-8 text.** Every format in §8.1
  is textual. A plugin needing to produce a binary blob writes it as a resource
  the host copies, not through this map.
- **Paths are relative to a config root the host resolves**, so the map is
  portable between a portable install and `%APPDATA%`, and a plugin cannot write
  outside the root. Lowering rejects absolute paths and `..` components.

### 8.3 Host-owned environment

**[V]** `hook_environment()` (`snowglass-runtime/src/launch.rs:61`) already owns
`SNOWGLASS_SESSION`, `SNOWGLASS_STONE_LAYOUTS`, `SNOWGLASS_SDL_GAMEPADS`, the log
paths, `SDL_DYNAMIC_API`, `SDL3_DYNAMIC_API`, `VK_LAYER_PATH`, and
`VK_INSTANCE_LAYERS`, and `EmulatorConfig` has no `env` field at all
(`instance.rs:48`).

- **P3** — `EmulatorConfig` gains `env: BTreeMap<String, String>`, and `launch()`
  applies it *before* `hook_environment()` so host entries always win. A plugin
  cannot set a `SNOWGLASS_*`, `VK_*`, or `SDL*_DYNAMIC_API` name; lowering rejects
  those outright, so the failure lands at load rather than at launch. Host-wins is
  a single rule at one site, which is why `[env]` needs no per-entry policy.
- `SDL_GAMECONTROLLERCONFIG` is generated by realize from each distinct
  `SdlOutputLayout` (§6.2/P2), not authored by a plugin.

### 8.4 Launch descriptor

The JSON the emulator's JS module returns. The frontend passes it through
verbatim; nothing in it requires understanding an option path.

The JSON the emulator's JS module returns. Three explicit outputs — `config`,
`env`, `argv` — plus the launch facts the runtime needs. The frontend passes it
through verbatim; nothing in it requires understanding an option path, a target,
or an input element.

```jsonc
{
  "schema": 1,
  "definitionHash": "b3:9f2c…",
  "plugin": { "id": "dolphin", "version": "0.4.1" },

  "command": "C:/Emulators/Dolphin/Dolphin.exe",
  "argv": ["-b", "-e", "C:/games/game.rvz"],
  "configRoot": "C:/Emulators/Dolphin/User",
  "isolate": true,

  "config": {
    "Config/GFX.ini":     "[Hardware]\nVSync = True\nAdapter = 0\n…",
    "Config/Dolphin.ini": "…",
    "Config/Hotkeys.ini": "…",
    "Config/GCPadNew.ini":"…"
  },

  "env": {
    "SDL_GAMECONTROLLERCONFIG": "030000005e04…,Snowglass Virtual Gamepad (GAMECUBE_CONTROLLER),a:b0,…",
    "DOLPHIN_USER_PATH": "C:/Emulators/Dolphin/User"
  },

  "hooks": ["vulkan", "d3d11"],
  "surfaces": [{ "index": 0, "selector": "#screen" }],

  "ports": [
    { "port": 0, "sdlIndex": 0, "mode": "gamecube", "layout": "GAMECUBE_CONTROLLER" },
    { "port": 1, "sdlIndex": null, "mode": "wiimote-real",
      "transport": "bluetooth-hci", "device": "hci0:00:1f:32:…" }
  ],

  "actions": [
    { "action": "save_state", "transport": "dinput",
      "chord": { "modifiers": [], "key": { "virtualKey": 113, "scanCode": 60, "extended": false } } }
  ],
  "pointer": { "transport": "dinput-mouse" },

  "claims": [{ "kind": "bluetoothAdapter", "id": "hci0" }],
  "diagnostics": [{ "severity": "warning", "path": "video.adapter", "message": "…" }]
}
```

Design rules:

1. **`config`, `env`, and `argv` are the whole configuration output.** A host that
   writes every `config` entry under `configRoot`, applies `env`, and appends
   `argv` has applied the configuration completely. There is no fourth channel and
   no out-of-band handle to resolve.
2. **The frontend is a conduit.** It does not validate, merge, or interpret. The
   host validates shape on receipt: relative paths inside the root, no reserved
   env names, known hook ids, known inputspec layouts.
3. **`definitionHash` makes staleness detectable.** A descriptor built against a
   definition that has since been reloaded is rejected at launch with a specific
   error rather than launched with stale files.
4. **The JS composes; realize supplies.** `ctx.realize()` returns `config`, `env`,
   `argv`, `ports`, `actions`, `claims`; the JS module adds `command`,
   `configRoot`, `surfaces`, `hooks`, and the game-specific tail of `argv`.
   Argv assembly, install-layout discovery, and the backend→hook mapping are all
   genuinely code: which hook captures a given renderer is knowledge about
   snowglass, not a fact about the emulator's config file, so it lives in the TS
   module rather than in the definition document. Editing what realize produced
   is possible and unpoliced: the plugin author wrote both halves, so this is a
   correctness boundary, not a trust boundary. The host validates shape, not
   intent.

Consuming the descriptor is mechanical: write `config` under `configRoot`, `env` →
`EmulatorConfig::env` (P3), `argv` → `EmulatorConfig::command` tail, `hooks` →
`EmulatorConfig::hooks`, `ports[].layout` in `sdlIndex` order →
`EmulatorConfig::gamepad_layouts`, `ports` → an `InputOperation::ReplacePorts`
transaction on `InputHub`, `actions` → `send_hotkey` with a transport.

### 8.5 Merge needs the existing files, and JS does not have them

`emit` merges against the current on-disk contents (§8.1), so the host reads them
*before* calling realize and passes them in through `ExistingFiles` — the same
call that produced the descriptor. By the time `config` exists as a `path →
contents` map it is already merged, and writing an entry is a plain overwrite. The
host does not re-merge at write time, and JS never sees the pre-merge state.

The window between read and write is real but uninteresting: an emulator that is
not running is not editing its own config, and the host holds the session.

---

## 9. Resolvers

Two distinct roles.

**Authoring** — a zod-style builder that infers the value type and emits the
definition document at build time. Strictly better than hand-written TOML, costs
nothing architecturally.

**Runtime** — the shell's existing `deno_core` host, which already runs the game
layer (`bins/snowglass-shell-webview2/src/game_layer/deno/`). Resolvers are
another export of the same plugin module, not a second engine. This crate sees
only the trait:

```rust
pub trait ResolverRuntime: Send + Sync {
    fn resolve_domain(&self, id: &str, ctx: &ResolveCtx) -> Result<Vec<Choice>>;
    fn evaluate_predicate(&self, path: &OptionPath, ctx: &ResolveCtx) -> Result<bool>;
    fn compute_default(&self, path: &OptionPath, ctx: &ResolveCtx) -> Result<Value>;
}
pub struct NoResolvers;   // zero cost when the manifest declares none
```

```ts
// Each export is addressable from the definition document as
// `resolver:<exported name>`; there is no registry to keep in sync with it.
// `depends` lists the option paths whose values this resolver reads, which is
// what puts it in the right place in the resolution order (§8).

// Which backends exist is a property of the installed build, not of the package,
// so it is resolved rather than frozen into the document. Order is meaningful:
// the first choice is the option's default when it declares none.
export const resolveGraphicsBackend = {
  depends: [],
  async resolve(ctx) {
    const build = await ctx.host.get("emulator.build");
    return build.videoBackends.map(name => ({ value: name, label: LABELS[name] ?? name }));
  },
};

export const resolveShaderPacks = {
  depends: ["main.core.gfx_backend"],
  async resolve(ctx) {
    if (ctx.get("main.core.gfx_backend") === "Software Renderer") return [];
    const entries = await ctx.fs.list("Shaders");
    return entries.filter(e => e.name.endsWith(".glsl"))
                  .map(e => ({ value: base(e.name), label: pretty(e.name) }));
  },
};

export default definePlugin<Dolphin>({
  // returns the launch descriptor; ctx.realize() is an op into this crate
  async launch(ctx, game) {
    const root = await ctx.installRoot();            // portable vs %APPDATA%
    const { config, env, argv, ...plan } = await ctx.realize({ configRoot: root });
    return {
      ...plan,                                        // ports, actions, claims, …
      command: `${root}/Dolphin.exe`,
      configRoot: `${root}/User`,
      config,
      env,
      argv: [...argv, "-b", "-e", game.path],
      hooks: HOOKS[ctx.get("main.core.gfx_backend")]
             ?? fail("Snowglass requires Dolphin's Vulkan, D3D, D3D12 or OGL backend."),
    };
  },
});

// Which snowglass hook captures which Dolphin backend. This is knowledge about
// snowglass, not about Dolphin's config file, so it lives here rather than in
// the definition document. A backend this table does not cover is one the
// overlay cannot attach to, so the lookup failing is the check (§6.7).
const HOOKS = {
  Vulkan: ["vulkan"],
  D3D:    ["d3d11"],
  D3D12:  ["d3d12"],
  OGL:    ["opengl"],
};
```

`ctx` is the entire surface: `get()`, `host.enumerate()`, scoped `fs`, `realize()`.
No network, no process, no `EmulatorInstance`. That narrowness is what makes a
later swap to a WASM Component Model guest mechanical.

Note the existing game-layer ops (`op_emulator_launch`, `op_input_configure` in
`game_layer/deno/emulator.rs:78‑151`) *do* drive the instance directly. Those stay
as the overlay's runtime surface; the configuration plugin boundary is separate
and narrower, and `launch()` returns data rather than calling `op_emulator_launch`
itself.

---

## 10. Generated artifacts

| artifact | generated from | checked by |
| --- | --- | --- |
| `*.gen.d.ts` | definition TOML | tsc |
| keyname tables | per-emulator source audit | `applies_to` + build detection |

The previous design's `profiles/sdl.gamepad.toml` and `layouts/*.toml` are gone:
this crate depends on `snowglass-input` and reads `InputLayout` and
`SdlOutputLayout` directly, so there is no constant to hand-copy and no version
field to drift.

---

## 11. Module layout

```
src/
  lib.rs
  raw/         serde structs, 1:1 with the document
  ir/          Definition, OptionDef, TargetGraph, Expr, ContentHash
  lower/       raw → ir, diagnostics
  value/       Value, ValueStack, ResolvedValues, Resolution
  host/        HostFacts, CapabilityRegistry, PortFact
  realize/
  emit/        Node, Rewriter, merge, Origin
    codec/     ini, cfg, xml, json
  input/       element projection, fragments, port plans, mapping generation
  action/      platform VK table, chord lowering
  descriptor/  LaunchDescriptor (serde, versioned)
```

---

## 12. Implementation order

1. `raw` + `ir` + `lower` with the §4.1 reject list, and a snapshot test per
   rejection. Independent of everything else and the most expensive thing to
   retrofit.
2. `value` + `realize` against static domains only, `NoResolvers`, empty
   `HostFacts`.
3. `emit` + the `ini` codec, with round-trip tests: parse → merge → emit preserves
   foreign keys, comments, and hand edits.
4. `descriptor` — serialization plus host-side shape validation. At this point
   Dolphin's video section produces a real `GFX.ini` and a real descriptor.
5. `[env]` and `[[argv]]`, then P3 (`EmulatorConfig::env`) to consume them.
6. P1 + P2 in the SDL hooks, then `input/`: port plans, layout-filtered fragments,
   mapping generation.
7. P4/P5 (transport selectors on `send_hotkey` and pointer delivery), then
   `action/`.
8. Resolver ops in the shell's deno host; remaining codecs.

---

## 13. Open items

Closed by source inspection:

- ~~snowglass device identity constants~~ — name and vendor/product are constant,
  `GamepadButton` indices are compile-time; element *presence* and raw indices are
  layout-dependent (§6.1).
- ~~SDL2/SDL3 intersection~~ — they differ today; P1 makes them agree (§6.2).
- ~~pointer reachability~~ — three transports, `dinput-mouse` reaches Dolphin
  (§6.6).
- ~~Stone layout codegen~~ — replaced by a direct `snowglass-input` dependency.
- ~~the runtime reading Stone~~ — the `consolespec` crate is the vocabulary,
  compiled in and read directly; there is one end to end (§6).
- ~~unvalidated platform ids~~ — machinespec is the source of truth, and the
  generators check against it (§6.5).

Remaining:

1. **PPSSPP `NKCODE_` range** — does F13+ exist? Single-file read of
   `Common/Input/KeyCodes.h`.
2. **melonDS modifier encoding** — `Config.cpp`.
3. **PCSX2/DuckStation keyname table sharing** — shared InputManager suggests yes.
4. **Linux/macOS keyboard hooks** — required if Dolphin off Windows is in scope.
   Windows-first is a fine trade; Linux is real work.
5. **Dolphin profile-scoped hotkeys** — may need a repeated target keyed on
   profile name, the same construct as ports.
6. **Config root discovery** — the JS supplies `configRoot`, but portable-vs-roaming
   detection is per-emulator and needs a host capability (`emulator.install`) to
   avoid every plugin reimplementing it.
