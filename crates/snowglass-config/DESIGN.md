# snowglass-config — Design

Revision of the Snowflake configuration/input design, narrowed to what this crate
owns and corrected against the snowglass tree. Source-verified claims are marked
**[V]**; unverified assumptions **[?]**.

---

## 0. Scope

`snowglass-config` is a pure library: **`(definition, overrides, host facts) → (files, launch arguments)`**.

It owns:

- parsing definition documents into `Raw*` structs,
- lowering them to a validated arena IR,
- materializing an IR into a configuration instance against host facts and an
  optional resolver runtime,
- serializing that instance to configuration bytes and a launch descriptor.

It does **not** own:

- persistence — the caller hands in a flat override map and stores whatever comes back;
- the filesystem — `emit` returns file contents, the host writes them;
- process launch, IPC, or `EmulatorInstance`;
- the JS runtime — only the `ResolverRuntime` trait; `deno_core` lives in the shell.

Dependencies: `toml-spanner` (§3.0), `consolespec`, `indexmap`, `thiserror`, and —
once lowering lands — `blake3` and `smallvec`. No `tokio`, no `deno_*`, no
`windows`, and notably **no `serde`**: definition documents are parsed by
`toml-spanner`'s `FromToml` derive, which yields span-carrying diagnostics and
borrows out of an `Arena` instead of allocating.
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

### 0.2 Implementation status

The crate today is `error` + `toml` (the `Raw*` layer, `src/toml/`). Nothing
downstream of parsing exists yet. This document distinguishes three states, and
every section below is tagged:

- **[parsed]** — a `FromToml` struct accepts it and four plugin packages exercise it.
- **[authored]** — the generators emit it and the documents contain it, but no
  Rust type reads it yet.
- **[designed]** — neither parsed nor authored; the shape below is a proposal.

| area | state | where |
| --- | --- | --- |
| `[meta]`, `[bool_style.*]` | parsed | `toml/document.rs`, `toml/config.rs` |
| `[targets.*]` | parsed, recursively (§3.1) | `toml/config/mod.rs` |
| `[sections.*]` + `options` | parsed | `toml/config.rs` |
| multi-document merge | parsed | `toml/document.rs` |
| `[input]`, `[input.mapping]`, `[input.layouts.*]` | parsed | `toml/input.rs` |
| `[platforms.*.ports.*]` | parsed (§6.5) | `toml/input.rs` |
| `[actions]` | authored; no type at all, and no manifest lists it (§7) | — |
| static and resolver domains | parsed, via `OptionDomain` (§3.5) | `toml/config.rs` |
| `[env]`, `[[argv]]` | designed; no plugin authors them (§3.3) | — |
| `parent` on a target | parsed; not yet honoured by lowering (§3.1) | `toml/config/mod.rs` |
| `overrides` on a target | designed (§3.1) | — |
| `enabled_when`, `when_disabled`, `repeat`, `repr` | designed (§3.2, §4) | — |
| everything from `lower` onward | designed (§4, §5, §8) | — |
| `setupLaunch` and the instance model | designed (§5.1) | — |

**All four plugin packages parse completely**, and
`toml/document.rs::every_plugin_package_parses` asserts it: it reads each
`plugin.toml`, expands its `definitions` globs, merges the result, and checks
that sections, targets, bool styles, the input mapping, layouts, and ports all
came out non-empty. Asserting on substance rather than on `Ok` is the point —
every bug in §0.3 produced a perfectly successful parse of nothing.

`config/actions.toml` is not currently reachable: no manifest lists it in
`definitions`, so wiring `[actions]` into the schema (§7.2) means changing the
manifests too.

### 0.3 Malformed input is an error, not a default

`TableHelper::optional` records a conversion failure in the `Context` and then
returns `None`; `Vec` does the same per element. So a malformed optional field
produced an `Ok` value with the field silently absent, and a malformed array
element silently vanished. `Document::to` guards against exactly this —
`Ok(v) if self.ctx.errors.is_empty()` — but §3.2.1's merge cannot use it, because
the merged table is not a `Document`, so `try_from_tomls` drove `from_toml`
directly and never read the context back.

It now does: any error the context recorded is returned as
`Error::DefinitionError`, whatever `from_toml` itself returned. Four cases that
previously parsed clean, all covered by tests in `toml/document.rs`:

| document | was | now |
| --- | --- | --- |
| `domain = { nonsense = 3 }` | `domain: None` | error |
| `range = { min = "not an int", max = 5 }` | `range: None` | error |
| `emit = { keyy = "K" }` | **the option vanished from the array** | error |
| a section table with no `title` | **the entire `sections` map emptied** | error |

The last two mattered most because the loss was not local to the bad field. Note
the fourth is only an error when nothing supplies the title: a `[sections.gfx]`
stub carrying no `title` is legal on its own, because §3.2.1's merge may supply
it from another document before the schema runs.

Errors accumulate rather than short-circuiting, so one pass reports every
violation in the package instead of the first — which is what makes the check
usable against a regenerated plugin, where a generator change tends to break
many options identically.

**Turning the check on exposed three parser bugs that had been invisible:**

- **`[bool_style.*]` had never parsed.** `BoolStyle` spells its fields as the
  raw identifiers `r#true`/`r#false`, and the derive does not strip the `r#`, so
  it looked for keys literally named `r#true`. Every table failed, and
  `#[toml(default)]` on `bool_styles` turned that into an empty map. Fixed with
  `#[toml(rename = "true")]`. Since §3.4 makes an undeclared `bool_style`
  reference a lowering error, this would have failed *every* boolean option in
  every package the moment lowering was written.
- **`OptionValue` had no `Float`.** `type = "float"` options exist and their
  defaults are floats (`default = 2.2`), so 22 defaults were being dropped.
- **`Emit` had no `int_style`.** Integer emits can carry a stdlib-style format
  string such as `0x{:08x}`, which lowers to §4's `Repr::Int { style }`.
  It was an unknown key.

**Untagged enums.** `OptionDomain` and `OptionValue` both need
`#[toml(FromToml, untagged)]`. Without it the derive expects an
externally-tagged representation that no document uses, so *every* variant fails
to match — and where the field is `Option<…>` that failure used to be silent.
Any future sum over document shapes needs the same attribute, and the
missing-`untagged` failure looks exactly like "this field is never set".

**What remains unmodeled.** With the above fixed and `[platforms]` wired in
(§6.5), every package parses as one document. `[actions]` (§7.2) is the only
block still without a type, and no manifest lists `config/actions.toml` in its
`definitions`, so nothing tries to parse it yet.

---

## 1. Data flow

```
plugin package (TOML + TS)
        │ parse
        ▼
    Raw* ──lower──► Definition (arena IR, content-hashed)   [once, at load]
                          │
   HostFacts ────────────►│ materialize()                    [per launch / per UI edit]
   dyn ResolverRuntime ──►│
                          ▼
                     Instance + Diagnostics
                          │ apply(configOverrides), launch script reads/writes
                          ▼
                     Instance (settled)
                          │ serialize(existing bytes)
                          ▼
          Realized { config, env, argv, ports, actions, claims }
                          │ launch script composes
                          ▼
          LaunchArguments ──► frontend ──► launch op ──► host writes, launches
```

The frontend is a conduit. It receives an opaque JSON object from the emulator's
JS module and hands it to the launch op unchanged; it never interprets an option
path, a target, or an input element.

---

## 2. Plugin package

```
plugins/dolphin/
  plugin.toml                     manifest
  config/dolphin.toml             meta, bool styles, file targets
  config/input.toml               input targets, mapping, layouts, ports
  config/actions.toml             hotkey target and action map
  config/sections/main.toml       one document per top-level section
  config/sections/gfx.toml
  config/sections/gfx/*.toml      one per subsection, where a section is large
  config/resolvers.ts             optional; exports are addressed as `resolver:<name>`
  support/parse_config.py         regenerates the documents from emulator source
```

```toml
[plugin]
id = "dolphin"
version = "0.0.1"
class = "emulator"

[emulator]
platforms = ["NINTENDO_GCN", "NINTENDO_WII"]

[emulator.configuration]
definitions = [
    "config/dolphin.toml",
    "config/input.toml",
    "config/sections/main.toml",
    "config/sections/gfx.toml",
    "config/sections/gfx/*.toml",
]
resolver = "config/resolvers.ts"    # omit → NoResolvers, no JS runtime is created
```

**A definition is a list of documents, not one document.** `definitions` is an
ordered list of package-relative paths, each of which may be a glob; the host
expands it and hands the crate an iterator of strings, in list order, with each
glob's expansion sorted. §3.2.1 describes how they combine.

`[capabilities]` is **[designed]** — no manifest declares one yet. When it lands,
capabilities are enforced by the host `CapabilityRegistry` and a resolver cannot
reach anything the manifest did not request:

```toml
[capabilities]
host = ["gpu.adapters", "display.modes", "emulator.build"]
fs   = ["plugin-dir:read"]
```

Note that the manifest itself is **not this crate's business** — nothing in
`snowglass-config` parses `plugin.toml`. The host reads it, resolves the globs,
and passes the text in.

---

## 3. Definition document

### 3.0 The parser

Documents are parsed by `toml-spanner`, not `serde`. Three properties matter to
the design and one of them is load-bearing:

1. **Spans survive.** A `Context` carries the byte range of every key and value,
   so a diagnostic can name the offending key *and point at it* — which is what
   principle 4 is actually asking for. `serde`'s data model discards position.
2. **The `Raw*` layer borrows.** Every struct in `src/toml/` is generic over
   `'de` and holds `&'de str`, allocated out of an `Arena` the caller owns. A
   Dolphin package is ~2.5 MB of TOML across thirty documents; parsing it
   allocates one arena and no strings. This is why lowering (§4) is where owned
   data first appears — the `Raw*` layer is a view over the source text.
3. **Custom collectors.** `#[toml(flatten, with = …)]` lets a struct absorb the
   keys a fixed schema does not name, with a per-key hook that runs *at parse
   time and with a span in hand*. That is how `[input.layouts.X]` parses each
   key straight into a `consolespec::BindingKey` (`toml/with.rs`), so a
   malformed binding key fails at its own line rather than surviving as a string
   nobody validates. §0.1's principle 6 stops being a convention and becomes a
   type.

The cost is that error handling is explicit rather than derived, which is
exactly where the §0.2 defect comes from.

### 3.1 Targets

A target is a place inside a configuration file. Nesting is the structure, and
TOML already has nesting — so the common case declares no relationships at all:

```toml
[targets.gfx]
output = { format = "ini", path = "Config/GFX.ini" }

[targets.gfx.Hardware]
[targets.gfx.Settings]
[targets.gfx."Color Correction"]
```

The table path **is** the target id, and its last component **is** the emitted
node name — always, with no way to separate them. Options emit to
`"gfx.Hardware"`; a section whose INI name is a valid bare key needs no body
whatsoever, and one whose name is not gets quoted (`[targets.gfx."Color
Correction"]`).

An earlier revision had a `node` field for the case where the id and the emitted
name should differ. It is **removed**: nothing in any package used it, quoting
already covers names that are not bare keys, and the one argument left for it —
two shared subtrees (below) both writing `<video>` under different parents —
is hypothetical. Keeping id and name identical means there is no second name to
keep in sync, and it can come back when a plugin actually needs it.

**Being a root and having an output are two different things.** A root is a
target with no parent — a positional fact, decided by where it is declared.
`output` says only that a root *serializes*, and a root may legitimately carry
none:

```toml
[targets.install]          # a null root: realized, never written
[targets.install.paths]
```

`output` is one table, `{ format, path }`, rather than two loose keys. A format
without a path is then unspellable rather than a lowering check, and "does this
serialize" is one `Option` rather than a pair that can disagree.

A **null root** is realized like any other — its options resolve, its values fold
through the override map — but it produces no entry in `Realized::config`, and its
subtree is instead visible to the emulator's launch script. That makes it the
place for bookkeeping the emulator never reads from a file: a computed install
layout, a value the JS needs when assembling `argv`, a group of settings the
plugin consumes itself. It is not a hack around the target tree; it is what the
target tree looks like when the sink is the launch descriptor rather than a file.

Consequently the schema cannot tell a root from a child, and does not try:
`Target::output` answers only "does this serialize", and rootness is established
by the walk over `targets`. An earlier revision had `Target::is_root()` returning
`format.is_some() && path.is_some()`, which was wrong twice — it conflated the
two properties, and a `Target` value has no idea whether it is top-level.

Depth is unbounded — XML and JSON formats nest further than INI's single level,
and Cemu's `settings.xml` uses four (`settings.content.Graphic.Overlay`).

The reference to a target — an option's `emit.target`, a section's `target`, a
port's `target` — is a dotted string parsed into `TargetPath`
(`toml/config/path.rs`), which precomputes its separator offsets and implements
`PartialOrd` as *ancestry*: two paths compare only when one contains the other.
That is the right relation for the DAG below, where siblings are genuinely
unordered.

`Target` is **recursive** — `output`, `parent`, and a flattened
`children: BTreeMap<&str, Target>` — so a target tree is walked rather than
collected, and each node's id is accumulated on the way down. That shape
replaced a `#[toml(flatten, with = with::target_paths)]` collector that was
wrong three ways, all of which the recursion removes rather than fixes:

- it recursed with `key.as_str()` rather than an accumulated path, so
  `targets.settings.content.Audio` was collected as the bare `"Audio"`;
- it pushed only *leaf* tables, so an interior node that is itself a target —
  `settings.content.Graphic`, which has both options of its own and a
  `Notification` child — was never collected at all;
- it opened with `let Some(table) = item.as_table() else { return; }`, silently
  skipping any non-table key. That is the §0.3 swallow again, and it would have
  eaten a misspelled `parent`. `BTreeMap<&str, Target>` instead reports
  `expected a table, found integer at 'targets.gfx.nonsense'`.

The cost is that `output` is optional, which the null root above requires
anyway. What moves into lowering is only the rule that a *child* may not declare
one — a top-level target with neither `output` nor `parent` is a null root, not
an error.

`overrides` is **[designed]**; `parent` parses but lowering does not yet honour
it.

`parent` exists **only** for the case nesting cannot express: one subtree
reachable from more than one root, such as settings duplicated across RetroArch's
core and global configs. Such a target is declared at the top level and names its
parents:

```toml
[targets.video]
parent = ["retroarch-core", "retroarch-global"]
```

It is a **list**. A single parent would express nothing nesting cannot, and
§3.2.1's merge already lets any document write `[targets.a.b.c]` directly;
several parents is the whole reason the field exists.

The declaration is named for the node it writes — `video`, not `shared-video` —
because the id is the emitted name. Its own children nest under it normally, so
`targets.video.scaling` materializes at `<video><scaling>` beneath every parent.

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
domain = { source = "resolver:resolveGraphicsBackend" }  # §3.5, [authored]

[sections.video.advanced]
title = "Advanced"                       # target inherited from `video`

[[sections.video.advanced.options]]
key     = "adapter"                      # path: video.advanced.adapter
title   = "Adapter"
type    = "select"
emit    = { target = "gfx.Hardware", key = "Adapter" }
domain  = { source = "host:gpu.adapters", value = "uuid", label = "name" }   # [designed]
default = { strategy = { first_where = "discrete == true" } }               # [designed]
enabled_when = "video.backend != 'Software'"                                # [designed]
```

The parsed shape is `OptionDefinition` in `toml/config.rs`. `key`, `title`,
`type`, and `emit` are required; `range`, `default`, and `domain` are optional.
`type` is one of `bool`, `int`, `float`, `string`, `select` — a bare `&str` today,
and a closed enum in the IR. `enabled_when`, `when_disabled`, `description`, and
`repeat` are **[designed]**: nothing parses them and no generator emits one.

`target` resolves from the nearest ancestor that declares one; `emit.target`
overrides it per option. A section holding only subsections is a legitimate
grouping and needs no `target` of its own — `[sections.gfx]` in Dolphin is
exactly that, a title and nothing else.

**`emit` is required.** The design previously said an option with no `emit` is
UI-only. No generator produces one, and making the field optional would mean the
common typo — a misspelled `emit` table — parses as a UI-only option and writes
nothing, silently. If a UI-only option is ever needed it should say so
(`emit = false` or `ui_only = true`), not say nothing.

**An option may also have no `default`**, and that is not the same as having one.
Some emulators do not keep their defaults anywhere a generator can read: PCSX2
sets them in the struct constructors its settings macros wrap, so its definition
has 358 keys and no values. Such an option is *unset* until the user picks
something, and an unset option **emits nothing at all** — the key stays absent
and the emulator applies its own default, which is the only honest thing to write
when we do not know what that default is.

The previous design added "…except for a static domain, where lowering rejects a
missing default, because the first choice would be an arbitrary pick."
**Withdrawn.** Dolphin's `main.core.fallback_region` is a `select` over six
static choices with no default, because Dolphin's own default lives somewhere
`parse_config.py` cannot read — and this is common, not exceptional. The rule
would reject correct documents. An unset select is unset, its first choice is a
*display* convention for the UI, and it emits nothing until the user picks. The
one thing that does stay a lowering error is `when_disabled = "default"` on an
option that has no default, since that one has no coherent behavior at all.

Options stay an array of tables rather than becoming keyed tables like their
section. The path would be marginally tidier, but options need no nesting and an
array is the natural shape for a flat list.

**Option order within a section is document order; section order is
lexicographic.** The previous design argued for `IndexMap` throughout so that
sibling section order would be document order. That does not survive §3.2.1:
sections arrive from thirty separate files merged into one table, so "document
order" names nothing. `Section::sections` is therefore a `BTreeMap` and siblings
sort by key. Options keep their order because they are an array inside one
document, where the question is well posed. `IndexMap` survives only where order
is genuinely meaningful and genuinely local — `LayoutMapping::mapping`, §6.5.1.

A settings UI that wants a curated page order will need to say so explicitly
(a `weight`, or an ordered list of children on the parent). Deriving it from the
filesystem was never going to hold.

### 3.2.1 Split documents — merge, not import

The previous design gave a section an `import` field naming a file to splice in
at that point. **That mechanism is gone**, replaced by something with no
mechanism at all: the documents in `definitions` are **merged into one table
before any schema is applied**, and a document is an ordinary rooted definition
that states the full path of whatever it contributes.

```toml
# config/sections/gfx.toml
[sections.gfx]
title = "Graphics"
```
```toml
# config/sections/gfx/hardware.toml
[sections.gfx.hardware]
title  = "Hardware"
target = "gfx.Hardware"

[[sections.gfx.hardware.options]]
key = "vsync"
title = "Vsync"
type = "bool"
default = false
emit = { key = "VSync", bool_style = "upper" }
```

`ConfigurationToml::try_from_tomls` (`toml/document.rs`) parses each text to a
raw `Table`, then folds it into an accumulator: two tables at the same path are
merged recursively, and any non-table — a scalar, an array, an inline table —
**replaces** whatever was there. Later documents win. The merged table is
handed to `FromToml` exactly once, so the schema sees a single document and
knows nothing about how many files produced it.

This is strictly simpler than importing and it is what the generators wanted
anyway. Three consequences worth stating:

- **There is no splice point and no placeholder.** A file does not need to be
  mentioned by the section it extends; `definitions` lists it and the path in
  the file does the rest. `config/sections/gfx/*.toml` is one glob, and adding a
  subsection is adding a file.
- **Order matters only for conflicts.** Merging is otherwise commutative, so the
  order of `definitions` is a tiebreak rule rather than a structure. It is a
  *sharp* tiebreak, though: an `options` array in a later document replaces the
  earlier one wholesale rather than appending, because arrays are not tables.
  Two documents contributing options to the same section is therefore a silent
  loss, and lowering should reject it (§4.1).
- **Cycles are impossible**, since nothing refers to anything. The import
  design's cycle check and its chain diagnostic are both moot.

The cost is that the merge happens below the schema, so a stub like
`[sections.gfx]` in one file and `[sections.gfx.hardware]` in another combine
into a section that is only valid *after* merging — which is correct, but it
means no single file is independently checkable, and the §0.2 defect makes a
missing `title` on such a stub fatal-but-silent rather than diagnosed.

`options` remains a reserved subsection name (§4.1): a table under a section is a
subsection, and the `options` array is what holds its controls.

`repeat` is **[designed]** — general over controller ports, memory card slots and
per-player blocks, expanding at lower time into N sections with paths
`port.1.mode`, `port.2.mode`. Note that ports specifically did *not* end up using
it (§6.5): every port is written out explicitly, because the emulators' numbering
conventions differ enough that a template would have to encode each one.

### 3.3 Environment and arguments — [designed]

Two explicit blocks, producing the two non-file outputs directly. Nothing parses
these and no plugin authors them; they are step 5 of §12 and the shape below is
unchanged from the previous design.

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

### 3.4 Bool styles — [parsed]

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

| kind | source | state | plugin code? |
| --- | --- | --- | --- |
| static | inline array of choices | parsed | no |
| host | `host:gpu.adapters` via capability registry | designed | no |
| resolver | `resolver:resolveGraphicsBackend` → TS module export | parsed | yes |

A static domain is an array of `{ key, label, value }` triples:

```toml
[[sections.main.core.options]]
key    = "fallback_region"
title  = "Fallback Region"
type   = "select"
domain = [
  { key = "NTSC_J", label = "NTSC-J", value = "0" },
  { key = "NTSC_U", label = "NTSC-U", value = "1" },
  { key = "PAL",    label = "PAL",    value = "2" },
]
emit   = { key = "FallbackRegion" }
```

**The triple, not the pair, is the important correction to the previous design.**
It had `Choice { value, label }`, where `value` was both what gets stored and
what gets emitted, and then had to add a rule — "`value` must be stable across
host state changes, and lowering warns when it names something ordinal" —
to stop a driver reshuffle from silently repointing saved config. The generated
documents solved it structurally instead:

- **`key` is the stable identity.** It is what the instance persists
  (§5), what an `Expr` compares against, and what a `Repr::Mapped` arm names. It
  is symbolic and it never changes because the emulator renumbered something.
- **`value` is what gets written to the file.** Dolphin's enums are ordinals, so
  it is usually `"0"`, `"1"`, `"2"` — precisely the thing that must not be a
  storage key.
- **`label` is display only** and never persists.

That splits the two jobs `value` was doing, and the ordinal-stability warning
becomes unnecessary: an ordinal in `value` is now the *expected* case, and there
is nothing to warn about. It also makes `Repr::Mapped` (§4) largely redundant for
selects, since `key → value` is already the map.

A resolver domain has a different shape entirely — a table, not an array:

```toml
[[sections.main.core.options]]
key    = "gfx_backend"
title  = "Gfx Backend"
type   = "select"
domain = { source = "resolver:resolveGraphicsBackend" }
emit   = { key = "GFXBackend" }
```

The two forms are one untagged sum, `OptionDomain` — `Select(Vec<SelectDefinition>)`
against the array, `Resolve { source }` against the table. Untagged matters here
(§0.2): without it neither variant matches, and `Option<OptionDomain>` turns that
into a silent `None` rather than an error.

A resolver returning choices must return the same triple, so a resolved domain
and a static one persist identically — that is what makes "the domain moved from
inline to resolved" a non-migration for stored values.

`source` stays a `&str` at this layer and splits into scheme and name during
lowering, where `resolver:` can be checked against the manifest's module exports
and `host:` against the capability registry (§4.1). Parsing it into a sum earlier
would put knowledge of the schemes in the document layer, which has no way to
validate either half.

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
    pub key: Arc<str>,                // stable identity — stored, compared, mapped
    pub value: Value,                 // emitted
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
`key` carries the stability requirement the previous design placed on `value`, so
a host record projected into a domain must project its *stable* field — a GPU's
UUID — into `key`, and may put whatever the emulator wants into `value`.

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

## 4. IR and lowering — [designed]

None of this exists. It is the next thing to build, and the shapes below predate
the `toml/` layer — read them as intent, to be reconciled against what §3 and §6
actually parse.

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
    Int { style: Arc<str> },
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
- a nested target that declares `output` — serializing is a property of a root,
  and a root is any target with no parent, with or without an output (§3.1)
- a child table whose key collides with a target field name (`format`, `path`,
  `output`, `parent`, `overrides`) or a section field name (`title`, `target`,
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
- a duplicate `key` among a domain's choices, or among a section's options
- an option whose `default` is not one of its static domain's `key`s (§3.5 —
  the default names a key, never a `value`)

Environment and arguments:
- two root targets writing the same file path without `overrides`
- an `[env]` name matching a host-owned variable or reserved prefix (§8.3)
- an `[env]` or `[[argv]]` entry referencing an undeclared option path
- an `[[argv]]` entry with a `flag` but no `value`

Document set (§3.2.1) — new, and only meaningful because merge replaced import:
- two documents contributing an `options` array to the same section path. Arrays
  replace rather than append, so the earlier one is lost silently. This is the
  one merge hazard the model introduces and it must be caught; it needs the
  merge to record which document last wrote each path.
- a section reached by the merge that carries no `title` — today the whole
  `sections` map silently empties instead (§0.2)

Input (§6):
- a port index that is not a decimal integer, or two ports in one group with
  indices that collide after parsing (`0` and `00`)
- a `[platforms.<id>]` key that is not a machinespec id, or is one the manifest's
  `platforms` list does not declare
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

## 5. Values and the configuration instance

### 5.1 The launch flow

The frontend drives a launch through one call, whose shape the rest of this
section is written against. `setupLaunch` itself is **[designed]** — the
signature below is settled, its implementation is not:

```ts
Emulator.setupLaunch({
  launchPath: string,
  emulator: string,                      // selects the launch script
  configOverrides: { [path: string]: Value },   // flat option path → value
  console: string,                       // consolespec machine id
  portAssignments: { [group: string]: string[] },  // inputspec layout id per port index
}) -> LaunchArguments
```

and the steps behind it:

```
definitions ──parse──► Definition ──materialize──► Instance   (defaults)
                                                      │
                            configOverrides ──apply──►│
                                                      │
                        launch script reads values ◄──┤        (any target, incl. null)
                                                      │
                                                  serialize
                                                      ▼
                                    { path: ArrayBuffer } per serializing root
                                                      │
                            launch script returns ────┴──► LaunchArguments
```

```ts
// minimally; extended as §8.4's other channels land
{ command: string, argv: string[], config: { [path: string]: ArrayBuffer } }
```

Three consequences that overturn earlier revisions of this document.

**The machine is a parameter, not a discovery.** `console` names the machinespec
id, so a `Definition` is lowered once per plugin and covers every machine it
declares, while an `Instance` is per machine. Ports belonging to other machines
never materialize. That is what keeps `Definition::hash` stable — it is a pure
function of the documents (§3.2.1) and of nothing about this launch.

**Port assignment arrives from the caller.** `portAssignments` gives a layout id
per port index within each group, which is exactly the domain of the implicit
`layout` option (§6.5.2). So that option stays, and `setupLaunch` *seeds* it
rather than replacing it: `emit.<choice>` selection, layout-option enablement,
and persistence all keep working through one mechanism instead of two.

**Config output is bytes, not text.** §8.2 previously required `String`
contents and told a plugin needing a blob to route it around this map. The
frontend takes `ArrayBuffer`, so that restriction is withdrawn: a codec returns
bytes, encoding is the codec's business, and an emulator wanting UTF-16, a BOM,
or a binary section is expressible rather than special.

### 5.2 The instance

An `Instance` is a `Definition` plus a value per option. Materializing one seeds
every option from its default; applying `configOverrides` writes over that.

**This replaces the five-layer `ValueStack`** of an earlier revision
(`[defaults, user profile, per-game, runtime overrides, system-forced]`). The
frontend owns persistence (§0), so it also owns *folding* — by the time a launch
happens it has already decided what the effective value is, and hands over one
flat map. A stack inside this crate would be a second place to resolve
precedence that nothing can observe.

What that costs is provenance: the crate can no longer distinguish "the user set
this" from "the system forced it", so a UI cannot say *forced by system* on the
strength of anything here. If that is wanted, the honest way is a tagged value in
`configOverrides` rather than reinstating the stack, since the frontend is where
the tag is actually known.

```rust
pub struct Instance {
    pub definition: Arc<Definition>,
    pub machine: MachineId,
    pub values: Vec<Value>,             // indexed by OptionId
    pub origin: Vec<Origin>,            // Default | Override, parallel to `values`
}

pub enum Resolution {
    Matched(ChoiceIndex),
    Orphaned { stored: Value, effective: Value },
}
```

An **orphan** — a stored value whose domain member has vanished — is still kept
as stored rather than collapsed to the effective value, so unplugging the
configured GPU, launching on the iGPU, and plugging it back in returns the
setting (§0.1, principle 5). And the caller must key persisted values against
`Definition::hash`, never against anything resolved, or saved config depends on
which GPU was plugged in when it was saved.

**The launch script reads the instance directly**, by option path, before
serialization — which is what makes a null root (§3.1) work without any new
channel. A value at a null target is materialized, overridable, and readable
exactly like any other; it simply contributes nothing to the serialize step. An
earlier draft of this document asked how such a subtree should reach the launch
descriptor. It does not: the script reaches *it*.

That makes `OptionPath` the scripting API as well as the persistence key, so
`Definition::by_path` is load-bearing and paths are a compatibility surface —
renaming one breaks saved config *and* every launch script that reads it.

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

**A port is not a section.** The previous design proposed making it one, with
`input = true` as the marker, and called reconciling that with the generators'
`[platforms.…]` spelling "the open work of §6.4". **That is now closed the other
way: `[platforms.…]` won** (§6.5), and `input = true` is withdrawn.

The reason is that the two things a port needs are things a section does not
have. A port is keyed by *(machine, group, index)* — it exists only when that
machine is being launched, and only within a group the machinespec declares — so
a port that lived in the section tree would have to carry a machine id, a group
id, and an index as fields, and lowering would have to check that trio against
consolespec anyway. Nothing about the section tree helps with that, and putting
ports in it would mean every consumer walking the tree has to know that some
nodes are not settings pages. Meanwhile the settings UI wants a port to look like
a page, which it can do by *rendering* one — it does not require the port to be
stored as one.

What survives from the `input = true` proposal is its substance, now attached to
the port declaration instead:

1. `{index}` and `{name}` expand in what a port emits, where `{index}` is the
   virtual pad slot (§6.3, *not* the port number) and `{name}` the pad's name for
   that port's layout. These live in `[input].device` now rather than being
   per-section — see below.
2. A port's emissions are **filtered by its selected layout**, so a control the
   chosen device does not have is never written.
3. A port has the implicit `layout` option of §6.5.2, whose domain is `layouts`
   plus `unplugged` and, where declared, `passthrough`.

**The `[input]` block header carries the two facts that are per-emulator rather
than per-layout** — which driver to hook, and how that emulator spells a device
reference:

```toml
[input]
driver = "sdl3"                   # sdl3 | sdl2 — selects the hook, and gates sensors
device = "SDL/{index}/{name}"     # the emulator's device-reference syntax
```

`device` is the template §6.4 previously described in prose as the fixed string
`SDL/0/Snowglass Virtual Gamepad`. Making it a template does two things: it
carries the port skew of §6.3 explicitly through `{index}`, and it lets P2's
per-layout pad rename (`{name}`) propagate without any plugin edit. An emulator
that spells device references differently states so here rather than needing a
dialect flag.

`driver` replaces the previous design's `EmulatorBuild::sdl_major` *detection*
for this purpose. The host still detects the build's SDL major from its import
table — that is a fact about the installed binary — but which hook to install is
a plugin statement, and disagreement between the two is a launch-time error worth
naming rather than something to silently reconcile.

**What a plugin declares per layout** is the emulator's own configuration key for
each control (§6.5.1). The left side is a consolespec binding key, the right is
the emulator's vocabulary and is the plugin's business alone:

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
definition. The previous design proposed two positional arrays for it:

```toml
[input.sdl]                        # WITHDRAWN
buttons = ["Button S", "Button E", …, "Shoulder R", "Pad N", …]
axes    = ["Left X", "Left Y", "Right X", "Right Y", "Trigger L", "Trigger R"]
```

**That is now `[input.mapping]`, keyed by binding key rather than by position:**

```toml
[input.mapping]
"button.a"              = "Button S"
"button.b"              = "Button E"
"button.l"              = "Shoulder L"
"button.r"              = "Shoulder R"
"directional.n"         = "Pad N"
"analog.stick.left.x+"  = "Left X+"
"trigger.left"          = "Trigger L"
```

This is the better shape for three reasons, and it is worth being explicit about
them because the positional form looks more faithful to SDL:

- **It is the same vocabulary as everything else** (principle 6). `[input.mapping]`
  and `[input.layouts.X]` are both `BindingKey → &str` — the same
  `LayoutMapping` type, the same parse-time `BindingKey` validation, the same
  diagnostics. The left column of one is the SDL virtual gamepad *as a device*;
  the left column of the other is the emulated device. Two arrays indexed by
  `SDL_GAMEPAD_BUTTON_*` would have been a second, positional namespace living
  beside the first, and §0.1 spent a principle saying there is only one.
- **A positional array cannot express the sign split.** Dolphin names each half
  of an axis separately (`Left X+`, `Left X-`), so the axis array would need six
  entries carrying twelve names, in a convention the schema does not state.
  `analog.stick.left.x+` states it.
- **Absence is expressible.** An emulator that does not name paddles simply omits
  those keys; a positional array has to pad with placeholders and lowering has to
  guess whether a placeholder means "unsupported".

The composition is unchanged: realize builds the value from the plugin's layout
key table, the shared projection, `[input.mapping]`, and `[input].device` —
`"button.z" → button.r → "Shoulder R" → "SDL/0/Snowglass Virtual Gamepad:Shoulder R"`
— and nothing lives outside the document.

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

A port declaration is a `[platforms.<MACHINE>.ports.<group>.<index>]` table.
This is now normative (§6.4); `PortAssignment` in `toml/input.rs` is its type,
and the path spells the *(machine, group, index)* key directly:

```toml
[platforms.NINTENDO_GCN.ports.controller.0]
target  = "gcpad.GCPad1"
layouts = ["GAMECUBE_CONTROLLER"]
emit.unplugged           = [{ target = "main.Core", key = "SIDevice0", value = "0" }]
emit.GAMECUBE_CONTROLLER = [{ target = "main.Core", key = "SIDevice0", value = "6" }]

[platforms.NINTENDO_WII.ports.wii.0]
target      = "wiimote.Wiimote1"
layouts     = ["WII_REMOTE_CONTROLLER", "WII_REMOTE_NUNCHUCK_CONTROLLER"]
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

A port's choices are exactly: the inputspec layouts in `layouts`, `unplugged`
always, and `passthrough` when the port emits for it. There is no separate `kind`
field — naming a layout *is* choosing a managed port, and the emissions under
that key are what "this port is now that" means in the emulator's config. `emit`
entries without a `target` land in the port's own; the GameCube device-type key
lives in a different file, so it names one.

**Passthrough is an `emit.passthrough` entry and nothing else.** An earlier
revision also gave the port a `passthrough = { transport = "bluetooth-hci",
requires = [...] }` table, which has been removed from both the design and the
Dolphin generator. Two reasons:

- **It said the same thing twice.** A port that emits for `passthrough`
  supports passthrough; a port that does not, does not. A separate field could
  only agree redundantly or disagree, and lowering would have to decide which
  wins. Choices are declared exactly one way — by appearing in `emit` — which is
  already true of `unplugged` and of every layout.
- **Its contents were not the plugin's to state.** That a Wii Remote is reached
  over `bluetooth-hci`, and that doing so needs exclusive use of the host's
  adapter, are facts about the *device and the host*, identical for every
  emulator that passes a Wii Remote through. Principle 3: the host owns
  capability, the plugin owns intent. Dolphin's intent is `Source = 2`; the
  transport belongs beside the machinespec, and the `ResourceClaim` (§8.2) is
  derived from the port's group and chosen mode rather than copied out of each
  plugin that happens to mention it.

So `PortAssignment` carries `target`, `layouts`, and `emit`, and its `emit` map
is keyed by choice — a layout id, `unplugged`, or `passthrough`. Lowering checks
that every key is one of those three (a test in `toml/document.rs` asserts it
across all four packages today).

**Every port names its own target and its own keys, so no template arithmetic is
needed anywhere.** Dolphin numbers pad sections from 1 and SI devices from 0; a
`{n}`/`{n-1}` scheme would have to encode that, and every emulator's variant of
it. Twelve explicit port declarations are longer and cannot be wrong. Cemu makes
the case louder still: its ports are separate *files* (`controllerProfiles/
controller0.xml` … `controller7.xml`), each a root target of its own, and its
gamepad group and controller group are numbered from a single shared sequence —
`gamepad.0` is `profile-0`, `controller.0` is `profile-1`. No template expresses
that; eight explicit declarations do.

The platform keys must be machinespec ids that the manifest's `platforms` list
also declares, and lowering checks both.

`[platforms]` is a `BTreeMap<&str, PortTable>` on `ConfigurationToml`, keyed by
machinespec id. `PortTable` is a struct with the single field `ports` rather
than a third map level, so a misspelled `ports` is rejected where it is written
instead of parsing as a platform whose only input group is named after the typo.
Below it the nesting is two anonymous maps, `group → index → PortAssignment`,
which types the index as `&str`: lowering must parse it as an integer and reject
a non-numeric key, since `ports.controller.x` would otherwise merge cleanly and
mean nothing.

### 6.5.1 Layout bindings — [parsed]

What a control is *called* is a property of the layout, not of the port, so key
tables are declared once and referenced by every port that offers the layout.
A table parses into `LayoutMapping` (`toml/input.rs`) as an
`IndexMap<BindingKey, &str>` — keys are parsed into `consolespec::BindingKey`
at their own line via the `with::binding_map` collector, so a malformed or
misspelled key fails *there*, with a span, rather than being carried as a string
to be validated later or never. This is §0.1's principle 6 as a type rather than
a convention, and it is the part of §3.0's custom-collector machinery that
earns its keep.

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

**[V]** The generated Dolphin document confirms this precisely.
`WII_REMOTE_CONTROLLER` binds `analog.accelerometer.z+`; the nunchuk layout binds
*neither* that key nor an inherited copy of it, but `analog.accelerometer.right.z+`
for the remote's own accelerometer and `analog.accelerometer.left.z+` for the
nunchuk's — twelve keys where the parent had six, and not one of the parent's six
still valid. Inheritance would have silently produced six dead bindings and lost
six live ones.

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

### 6.5.2 Layout options — [parsed]

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

These are `OptionDefinition`s — the same type as a section's, with the same
required `key`/`title`/`type`/`emit` and the same optional `range` and `default`
— living in `LayoutMapping::options` beside the binding table. Dolphin authors
five for `WII_REMOTE_CONTROLLER` (MotionPlus, speaker pan, battery, upright,
sideways) and one for `GAMECUBE_CONTROLLER`.

Lowering expands each one across every port that offers the layout, producing an
option at `ports.<machine>.<group>.<index>.<key>` whose `emit.target` is that
port's target. So a four-remote Wii setup gets four independent `sideways`
settings, and their values persist per port rather than bleeding across them. The
path gains a group component relative to the previous design, for the reason
§6.5 gives: `wii.0` and `gamecube.0` are different ports.

Like the key table (§6.5.1), a layout declares its options in full — a nunchuk
port keeps the remote's options because `WII_REMOTE_NUNCHUCK_CONTROLLER` states
them again, not because it inherits them.

**Every port has an implicit option, `ports.<machine>.<group>.<index>.layout`**, whose
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
collected. The authored shape, **[authored]** by all four generators and parsed
by nothing:

```toml
# plugins/duckstation/config/actions.toml
[actions]
target    = "settings.Hotkeys"
transport = "win32-message"      # win32-message | dinput
keynames  = "duckstation"

[actions.map]
load_state        = { key = "f1",  expression = "Keyboard/F1",  name = "LoadSelectedSaveState" }
save_state        = { key = "f2",  expression = "Keyboard/F2",  name = "SaveSelectedSaveState" }
toggle_fullscreen = { key = "f11", expression = "Keyboard/F11", name = "ToggleFullscreen" }
```

Four corrections to the previous design:

- **`template` is gone; `expression` is per-action.** The design assumed a
  `"Keyboard/{key}"` template would render every binding. It does not survive
  chords: PCSX2 writes `toggle_fullscreen` as `Keyboard/Alt & Keyboard/Return`
  and Dolphin writes `@(Alt+RETURN)` — the emulator's *expression grammar*, not
  a key name substituted into a slot. So the generator, which is already reading
  that emulator's source, emits the finished expression. `keynames` survives as
  a dialect tag, but purely as provenance for the generator now, not as a table
  the crate indexes.
- **`key` and `expression` are two different things and both are needed.** `key`
  (`"alt+return"`, lowercase, snowglass's own chord syntax) is what §7.3's
  platform table turns into a `HotkeyKey` for the hook to synthesize.
  `expression` is what gets written into the emulator's file. They describe the
  same chord in two vocabularies and neither derives the other.
- **`device` joins `[actions]` where the emulator needs one.** Dolphin's hotkeys
  are qualified by input device (`device = "DInput/0/Keyboard Mouse"`), the same
  way its pad bindings are; the stenzek emulators are not.
- **`fixed = true` did not materialize; a partial entry did.** Cemu's map is
  `exit = { name = "ExitApplication" }` — a `name` with no `key` and no
  `expression`, meaning "this action exists and Cemu hardcodes its binding".
  Cemu also carries `encoding = "wx-keycode-pair"` on the block and `wx_key =
  "WXK_F12"` on the two entries it can actually write. So the entry is a sum:
  fully bound, name-only, or absent (the five actions Cemu has no equivalent for
  are simply not in the map, which is what §7.4 asks for).

`applies_to` is **[designed]** — the versioning of §7.5 is real but no generator
emits a version constraint yet, since none of them can determine a revision
(`source_revision = "unknown"` in every `[meta]`).

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

## 8. Materialize, serialize, and the descriptor

Two steps, matching §5.1's flow. An earlier revision called them `realize` and
`emit` and had them exchange a `RealizedDefinition`/`ResolvedValues` pair; the
`Instance` is that pair, named, and mutable between the two calls so the launch
script can read and adjust it.

```rust
pub fn materialize(
    definition: &Arc<Definition>,
    machine: MachineId,
    ports: &PortAssignments,
    facts: &HostFacts<'_>,
    resolvers: &dyn ResolverRuntime,
) -> Result<(Instance, Diagnostics)>;

impl Instance {
    pub fn apply(&mut self, overrides: &BTreeMap<OptionPath, Value>) -> Diagnostics;
    pub fn get(&self, path: &OptionPath) -> Option<&Value>;   // the launch script's view
    pub fn set(&mut self, path: &OptionPath, value: Value) -> Result<()>;
}

pub fn serialize(
    instance: &Instance,
    existing: &dyn ExistingFiles,     // borrowed bytes, host-supplied; no I/O here
) -> Result<Realized>;
```

`materialize` iterates `definition.order` — a toposort over `deps` and
`enabled_when` refs, so a cycle fails loudly at load rather than producing
different files on different runs. Per option: resolve domain, evaluate enable,
seed from default, commit.

`apply` is separate from `materialize` rather than an argument to it because the
frontend's overrides and the launch script's writes are the same operation, and
because a settings UI edits an instance repeatedly without rebuilding it.

An override naming a path the definition does not have is a **diagnostic, not a
rejection**: `configOverrides` may legitimately carry values saved against an
older definition whose option has since been renamed or removed, and principle 5
says not to destroy them. The caller decides whether a warning is fatal.

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
Instance ─────────┘
```

Foreign keys and comments survive. `Origin::Fragment` and `Origin::Forced` are
always snowglass-owned. Host formats: `ini`, `cfg`, `xml`, `json` — ~95% of
emulator config. Plugins export `emit` only for genuine oddities.

There is no `cmdline` codec. Arguments are rendered directly from `[[argv]]`:
there is nothing to parse, nothing to merge, and no foreign content to preserve,
so the round-trip machinery would be dead weight.

### 8.2 Serialize output

```rust
pub struct Realized {
    pub config:  BTreeMap<String, Vec<u8>>, // config-root-relative path → file bytes
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

`serialize` **returns** contents rather than writing them: sandboxing, unit tests
with no filesystem, and a free "preview generated config" feature. Unsatisfiable
claims fail *before* launch with a specific message, rather than after launch with
a silently dead controller.

Only roots carrying an `output` (§3.1) contribute an entry. A null root is
realized and readable through `Instance::get` but produces no file, so the size
of this map is a property of the definition, not of the values.

`config` is a flat `path → contents` map, not a list of structs, because that is
what it becomes on the wire (§8.4). Two consequences worth stating:

- **Contents are bytes.** They reach the frontend as `ArrayBuffer` (§5.1), so
  encoding is the codec's business rather than a constraint on the map. An
  earlier revision made these `String` and told a plugin needing a blob to route
  it around this channel; that ruled out UTF-16, a BOM, and any format with a
  binary section, for no gain now that the wire type takes bytes anyway.
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
4. **The JS composes; serialize supplies.** `ctx.realize()` returns `config`, `env`,
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

`serialize` merges against the current on-disk contents (§8.1), so the host reads
them *before* materializing and passes them in through `ExistingFiles` — the same
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
  error.rs     [exists]
  toml/        [exists] FromToml structs, 1:1 with the document — was `raw/`
    config.rs    targets, sections, options, bool styles
    config/path.rs   TargetPath
    document.rs  ConfigurationToml + the multi-document merge
    input.rs     [input], layouts, ports
    with.rs      custom collectors (binding keys, target paths)
  ir/          Definition, OptionDef, TargetGraph, Expr, ContentHash
  lower/       toml → ir, diagnostics
  value/       Value, Instance, Resolution
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

0. ~~Malformed input must be an error~~ — done (§0.3). ~~Wire `[platforms]` into
   `ConfigurationToml`~~ — done (§6.5). ~~`Target` must record full paths
   including interior nodes~~ — done, by making it recursive (§3.1). All three
   are guarded by `every_plugin_package_parses`.
1. Model `[actions]` and add `config/actions.toml` to each manifest's
   `definitions` (§7.2). At this point the documents the generators already emit
   are fully readable.
2. `ir` + `lower` with the §4.1 reject list, and a snapshot test per rejection.
   The most expensive thing to retrofit.
3. `value` + `realize` against static domains only, `NoResolvers`, empty
   `HostFacts`.
4. `emit` + the `ini` codec, with round-trip tests: parse → merge → emit preserves
   foreign keys, comments, and hand edits.
5. `descriptor` — serialization plus host-side shape validation. At this point
   Dolphin's video section produces a real `GFX.ini` and a real descriptor.
6. `[env]` and `[[argv]]`, then P3 (`EmulatorConfig::env`) to consume them.
7. P1 + P2 in the SDL hooks, then `input/`: port plans, layout-filtered fragments,
   mapping generation.
8. P4/P5 (transport selectors on `send_hotkey` and pointer delivery), then
   `action/`.
9. Resolver ops in the shell's deno host; remaining codecs.

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

Closed by the generated documents:

- ~~`input = true` sections vs `[platforms.…]` ports~~ — `[platforms.…]` won;
  ports are keyed by *(machine, group, index)* and are not sections (§6.4, §6.5).
- ~~the `[input.sdl]` positional arrays~~ — replaced by `[input.mapping]`, keyed
  by binding key like every other table (§6.4).
- ~~`import` splice points~~ — replaced by a pre-schema merge over a glob list
  (§3.2.1). Cycles, chains, and placeholder rules are all moot.
- ~~`IndexMap` for section order~~ — merging across documents makes "document
  order" meaningless for sections; they sort lexicographically and options keep
  their array order (§3.2).
- ~~ordinal `value` silently repointing saved config~~ — a choice is a
  `{ key, label, value }` triple, `key` persists and `value` is emitted (§3.5).
- ~~`template = "Keyboard/{key}"`~~ — chords do not survive substitution; the
  generator emits a finished `expression` per action (§7.2).

Remaining:

1. **Section presentation order.** §3.2's lexicographic sibling order is honest
   but arbitrary — Dolphin's Graphics page would list `colorcorrection` before
   `hardware`. If a curated order is wanted it now needs an explicit field
   (`weight`, or an ordered `children` list on the parent), because neither the
   filesystem nor the merge can supply it.
2. **What `keynames` is for.** With `expression` authored per action, the
   dialect tag no longer selects a table this crate indexes. Either it is
   generator provenance and belongs in `[meta]`, or §7.3's emulator-side table
   still has a job and it should say which.
3. **Merge provenance.** Catching the options-array clobber of §4.1 means the
   merge must remember which document last wrote each path. That is cheap to add
   now and awkward later.
4. **PPSSPP `NKCODE_` range** — does F13+ exist? Single-file read of
   `Common/Input/KeyCodes.h`.
5. **melonDS modifier encoding** — `Config.cpp`.
6. **PCSX2/DuckStation keyname table sharing** — shared InputManager suggests yes.
7. **Linux/macOS keyboard hooks** — required if Dolphin off Windows is in scope.
   Windows-first is a fine trade; Linux is real work.
8. **Dolphin profile-scoped hotkeys** — may need a repeated target keyed on
   profile name, the same construct as ports. Note Cemu already does exactly
   this for *ports* — one file per profile, each a root target (§6.5) — so the
   construct exists and the question is only whether hotkeys reuse it.
9. **Config root discovery** — the JS supplies `configRoot`, but portable-vs-roaming
   detection is per-emulator and needs a host capability (`emulator.install`) to
   avoid every plugin reimplementing it.
