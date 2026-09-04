# Plan — repeated sections, XML attributes, and a single-pass `onConfigure`

A handoff document. It replaces `PLAN-emit-attributes.md`, which is deleted.

Two deliverables. They are independent, and you can build them in either order.

- **A.** A definition document declares the **shape** of a repeated node, and the
  plugin makes the instances of that shape in `onLaunch`. The plugin supplies
  values only. It does not supply node names, nesting, escaping, or XML text.
  This deletes `injectGraphicPacks` (`plugins/cemu/cemu.ts:142-155`).
- **B.** `onConfigure` gives the layout and the settled values together, and the
  host stops calling it in a loop.

The rule `no code comments` from CLAUDE.md applies. Write bare code, and put the
reasons in the commit message or in DESIGN.md.

---

# Part A — repeated sections

## 1. Why

Cemu keeps the graphics-pack selection as repeated elements in `settings.xml`:

```xml
<GraphicPack>
    <Entry filename="graphicPacks/…/Brightness/rules.txt" disabled="false">
        <Preset><category>Aspect Ratio</category><preset>16:9</preset></Preset>
        <Preset><category>Shadows</category><preset>High</preset></Preset>
    </Entry>
    <Entry filename="graphicPacks/…/CrashFix/rules.txt" disabled="false"/>
</GraphicPack>
```

Verified against `C:/Users/ronny/AppData/Roaming/Cemu/settings.xml:59-88` and
against `D:/coding/cemu/src/config/CemuConfig.cpp:338-357` (the save) and
`:89-121` (the load).

The crate cannot make this today, for two reasons:

- **One target gives one node.** `Serializer::writes` is a `Vec<Vec<WritePlan>>`
  with an index of the target handle (`ser/serializer.rs:24`).
- **No emitter writes an attribute.** `EmittedValue` gives a key and a value,
  and `SectionRef` gives a path and a name (`ser/emitter/mod.rs:62`).

Thus the plugin builds the XML as text and puts it into the rendered document
with `lastIndexOf("</content>")` (`cemu.ts:147`). This is not acceptable:

- The plugin escapes the XML itself (`cemu.ts:157`), thus there are two
  implementations of the same rule.
- If the `content` element ever has no active write, `compute_active_targets`
  (`ser/serializer.rs:519`) omits it, and the search fails at launch time.
- The result is text, not a document. A later merge with a file of the user
  cannot see the packs.

## 2. The division of the work

| The definition document owns | The plugin owns |
| --- | --- |
| the node names `Entry` and `Preset` | which entries exist |
| the nesting below `settings.content.GraphicPack` | the order of the entries |
| which value is an attribute | the value of each option |
| the bool style, the int style | |
| the type of each option and its range | |

The plugin calls `append()` and then writes values through the usual options
proxy. It never names a node and it never escapes a character.

**Resolvers do not exist.** Do not add `items`, `depends`, or a source scheme.
The dynamic set comes from `onConfigure` and it reaches `onLaunch` in
`userConfiguration`. The plugin already computes it in
`plugins/cemu/graphics-packs.ts`.

## 3. The definition document

Targets. `repeat = true` is a new field:

```toml
[targets.settings.content.GraphicPack]

[targets.settings.content.GraphicPack.Entry]
repeat = true

[targets.settings.content.GraphicPack.Entry.Preset]
repeat = true
```

Sections. `repeat = true` is a new field here also:

```toml
[sections.content.graphics_packs]
title  = "Graphics Packs"
target = "settings.content.GraphicPack.Entry"
repeat = true

[[sections.content.graphics_packs.options]]
key   = "filename"
title = "Pack"
type  = "string"
emit  = { key = "filename", xml.attribute = true }

[[sections.content.graphics_packs.options]]
key     = "enabled"
title   = "Enabled"
type    = "bool"
default = true
emit    = { key = "disabled", xml.attribute = true, bool_style = "inverted" }

[sections.content.graphics_packs.presets]
title  = "Presets"
target = "settings.content.GraphicPack.Entry.Preset"
repeat = true

[[sections.content.graphics_packs.presets.options]]
key   = "category"
title = "Category"
type  = "string"
emit  = { key = "category" }

[[sections.content.graphics_packs.presets.options]]
key   = "preset"
title = "Preset"
type  = "string"
emit  = { key = "preset" }
```

Three points:

- `preset` is a `string`, not a `select`. The choices are dynamic, they live in
  the layout that `onConfigure` gives, and the user selects one there. The
  instance carries only the selected name. Thus no dynamic domain is needed.
- `enabled` writes the key `disabled`, because Cemu names the field that way.
  The bool style `inverted` gives the opposite text. Add it to `cemu.toml`:
  `[bool_style.inverted] true = "false"  false = "true"`.
- A repeated section is **not** in the settings-UI layout. `expand_layout`
  (`src/layout.rs`) reaches a definition option through a `source` reference,
  and a repeated option has no value until launch. Reject a `source` that names
  a repeated section, with the existing `UnknownSource` error.

The graphics-pack section is hand-written, thus it must not go into a generated
document. Put it in `plugins/cemu/config/sections/graphics-packs.toml` and add
that path to `emulator.configuration.definitions` in `plugins/cemu/plugin.toml`.
The existing glob `config/sections/content-*.toml` does not match it.

## 4. Instance keys are ordinals

A repeated section materializes **no** setting when the instance is built. The
plugin appends each instance, and the crate numbers them from zero in append
order:

```
content.graphics_packs.0.filename
content.graphics_packs.0.enabled
content.graphics_packs.0.presets.0.category
content.graphics_packs.0.presets.0.preset
content.graphics_packs.1.filename
```

Do **not** use the path of the pack as the key. Two reasons:

- `SettingPath` separates on a period (`ast/path.rs`), and a pack path contains
  a period and a slash.
- A stable key is not necessary here. The user values persist in `localStorage`,
  keyed by the layout group (`js/03_configuration.js`, `#storageKey`). The
  `ConfigurationInstance` is made again at each launch, thus its keys are
  private to one launch.

This is the same construction that `instantiate_ports` already uses
(`ast/instance.rs:86`), where the port index gives the path.

**Resolution of a path.** To find the section of an instance path, remove every
component that is a decimal integer. `content.graphics_packs.0.presets.1` gives
the section `content.graphics_packs.presets`. The host needs this to answer
"does this path repeat".

## 5. Step 1 — the TOML layer

File: `crates/polymer-config/src/toml/config/mod.rs`

Add `#[toml(default)] pub repeat: bool` to `Target` (`:13`) and to `Section`
(`:39`).

File: `crates/polymer-config/src/toml/config/options.rs`

Add the XML style, and one field to `Emit` (`:5-12`):

```rust
#[derive(Debug, Toml)]
#[toml(FromToml)]
pub struct XmlStyle {
    #[toml(default)]
    pub attribute: bool,
}
```

```rust
pub(crate) xml: Option<XmlStyle>,
```

`key` stays mandatory and stays `&'de str`. `xml.attribute` is a **style**, in
the same group as `bool_style` and `int_style`. It does not give a second name.

**Verify first:** confirm that `toml_spanner` accepts a dotted key inside an
inline table, `emit = { key = "filename", xml.attribute = true }`. TOML 1.0
permits it, but no document in this repository uses it. If the parser rejects
it, write `xml = { attribute = true }` and tell the user.

## 6. Step 2 — the lowered definition

File: `crates/polymer-config/src/ast/definition.rs`

- `SectionDefinition` (`:52`) gains `pub repeat: bool`.
- `EmitDefinition` (`:70`) gains `pub xml_attribute: bool`. Use a plain `bool`.
  `bool_style` keeps a name because it is resolved later; this flag has nothing
  to resolve.

File: `crates/polymer-config/src/ast/target.rs`

`TargetDefinition` (`:12`) gains `pub repeat: bool`.

File: `crates/polymer-config/src/ast/from_toml.rs`

- `lower_target` and `lower_section` carry the new field through.
- `lower_emit` sets `xml_attribute: emit.xml.is_some_and(|xml| xml.attribute)`.
  `ActionsDefinition::emit` uses the same function and needs no change.

## 7. Step 3 — the rules

The crate has no error channel in lowering. `From<ConfigurationToml>` panics on
bad input today (`lower_option`, `lower_platform`), and this plan does not
change that. Put each rule in `Serializer::new`, which already returns a
`Result`. Move them into a validation pass when DESIGN §4.1 lands.

1. **A repeated section must name a repeated target**, and the reverse. A
   mismatch is an error.
2. **A repeated target must have a parent.** The repeated element needs a
   container.
3. **An attribute needs an XML output.** Every output root that can reach the
   target must have the format `xml`. A target can have several roots, because
   `additional_parents` exists (`ast/target.rs:16`). Compute this once in
   `TargetGraph::compile` (`:51`); the topological order puts each parent before
   its children, thus one forward pass is sufficient. A target that reaches no
   output root gets `true`, because a null root writes no file and no emitter
   can receive the attribute.
4. **An attribute needs an enclosing element.** Reject an attribute when the
   target has `parent.is_none() && additional_parents.is_empty()`.
   `serialize_document` (`ser/serializer.rs:115`) writes the values of the root
   target before it enters any section.

New errors in `crates/polymer-config/src/error.rs`:

```rust
#[error("section `{section}` and target `{target}` disagree about repetition")]
RepeatMismatch { section: Box<str>, target: Box<str> },
#[error("repeated target `{0}` has no parent to contain it")]
RepeatWithoutParent(Box<str>),
#[error("target `{target}` writes format `{format}`, which has no attributes")]
AttributeUnsupportedFormat { target: Box<str>, format: Box<str> },
#[error("target `{0}` is a document root and has no element to hold an attribute")]
AttributeOnDocumentRoot(Box<str>),
#[error("section `{0}` does not repeat and cannot receive an instance")]
NotRepeatable(Box<str>),
```

## 8. Step 4 — the instance

File: `crates/polymer-config/src/ast/instance.rs`

**`instantiate_sections` (`:64`)** skips a section that has `repeat`, and it
skips every section below it. Walk the section tree instead of the flat arena,
so that a nested repeated section is reached only through its parent.

**Add two public methods:**

```rust
pub fn repeats(&self, path: &str) -> bool;
pub fn append(&mut self, path: &str) -> Result<Box<str>>;
```

- `path` is an instance path such as `content.graphics_packs` or
  `content.graphics_packs.0.presets`. Resolve it to a section by the rule of
  paragraph 4.
- `repeats` gives `true` when that section has `repeat`. The host bridge uses
  it, and it never fails.
- `append` checks `repeat` (`NotRepeatable`), builds the prefix `<path>.<n>`
  where `n` is the count of the existing instances at that exact path, appends
  one setting for each option of the section with its default, and gives the
  prefix back.
- `append` does not append the options of a nested repeated section. Those
  arrive through a later `append`.

Keep an ordered record, for example `instances: IndexMap<Box<str>, u32>` from
the instance path to the count. The serializer needs the append order, and the
arena order alone is not sufficient once two sections interleave.

`set_json` (`:242`) needs no change. It resolves by path, and an instance path
is a path.

## 9. Step 5 — the serializer

File: `crates/polymer-config/src/ser/serializer.rs`

**`WritePlan` (`:49`)** gains `attribute: bool`.

**The write store (`:24`)** changes:

```rust
writes: Vec<IndexMap<Option<Box<str>>, Vec<WritePlan<'a>>>>,
```

The outer index stays the target handle. The key is the instance prefix, and
`None` is the usual, non-repeated case. Use `IndexMap`, not `BTreeMap`: the
order must be the append order, and a `BTreeMap` puts `10` before `2`.

**`plan_section_writes` (`:161`)** walks the section tree. For a repeated
section it plans one group for each instance prefix that the instance holds, and
it builds the setting path from the prefix. For a usual section it behaves as
today, with the key `None`.

**`add_emit` (`:375`)** takes the instance key, sets `attribute` from
`emit.xml_attribute`, and applies rules 3 and 4 of paragraph 7.

**`write_section` (`:407`)** takes an instance prefix, so that a child knows
which parent it belongs to:

```rust
fn write_section(
    &self,
    handle: Handle<TargetDefinition>,
    prefix: Option<&str>,
    emitter: &mut dyn Emitter,
) -> Result<()>;
```

For a target that does not repeat, the behaviour is the behaviour of today, and
`prefix` passes down unchanged. For a repeated target:

1. Select each instance key of that target that begins with `prefix`.
2. For each key, in order:
   a. Render the attribute plans through `self.scalar(…)` (`:473`). Skip a plan
      that gives `None`, as `write_values` already does for `Value::Null`.
   b. Collect them into a local `Vec<EmittedAttribute<'_>>` and borrow it into
      `SectionRef`.
   c. `enter_section`, then the value plans of that key, then the children with
      this key as the prefix, then `exit_section`.

The vector is a local and it lives across all the calls. `SectionRef` stays
`Copy`, because a shared slice is `Copy`.

**`write_values` (`:432`)** takes the instance prefix, and it skips each plan
that has `attribute`. Keep the order: values, then bindings.

**`compute_active_targets` (`:519`)** examines every instance group, not one
list. A repeated target with no instance is not active, thus an empty
`<GraphicPack>` element is not written.

## 10. Step 6 — the emitters

File: `crates/polymer-config/src/ser/emitter/mod.rs`

```rust
#[derive(Clone, Copy, Debug)]
pub struct EmittedAttribute<'a> {
    pub key: &'a str,
    pub value: ScalarValue<'a>,
}

#[derive(Clone, Copy, Debug)]
pub struct SectionRef<'a> {
    pub path: &'a str,
    pub name: &'a str,
    pub attributes: &'a [EmittedAttribute<'a>],
}
```

`ScalarValue` does not implement `PartialEq`. Thus remove the `Eq` and
`PartialEq` derives from `SectionRef` (`:61`). Every `exit_section` compares
`section.name` directly, thus nothing else breaks.

Add `pub fn reject_attributes(&self, format: &str) -> EmitResult<()>` on
`SectionRef`. It gives `Error::emitter_message` when the slice is not empty.

**`ser/emitter/xml.rs`** — in `enter_section` (`:52`):

```rust
let mut start = BytesStart::new(section.name);
for attribute in section.attributes {
    start.push_attribute((attribute.key, attribute.value.render()?.as_ref()));
}
self.document.write_event(Event::Start(start))?;
```

`quick_xml` escapes an attribute value in `push_attribute`. Do not escape it
again.

The single-root check (`:53-60`) counts a root each time the section stack is
empty. A repeated target is never a root, thus the check still holds.

**`ser/emitter/ini.rs` and `ser/emitter/bml.rs`** — call
`section.reject_attributes("ini")` or `("bml")` at the start of
`enter_section`. Paragraph 7 makes this unreachable. It is a guard, and it keeps
the invariant local to the emitter.

**A repeated section in INI or BML is legal**, and both formats already accept a
repeated name. Do not reject it. Only the attribute is XML-only.

## 11. Step 7 — the host bridge

`append` is a method **on the options proxy at the path of the section**. It is
not a method on the configuration object, and it takes no path argument:

```ts
const entry = args.configuration.options().content.graphics_packs.append();
entry.filename = "graphicPacks/downloadedGraphicPacks/…/rules.txt";
entry.enabled = true;
for (const { category, preset } of pack.presets) {
    const node = entry.presets.append();
    node.category = category;
    node.preset = preset;
}
```

`append()` gives back an options proxy at the new instance path. A nested
repeated section is reached by an ordinary property read on that proxy. Thus one
mechanism covers both levels, and no wrapper object is needed.

File: `crates/runtime/polymer-pluginhost/deno.rs`

Add two ops beside `op_configuration_set` (`:99-103`):

- `op_configuration_repeats(id, path) -> bool`
- `op_configuration_append(id, path) -> String`

File: `crates/runtime/polymer-pluginhost/configuration.rs`

Add the two matching methods on `ConfigurationInstances`, in the shape of `set`
(`:85-98`).

File: `crates/runtime/polymer-pluginhost/js/configuration.js`

Change the `get` handler of the proxy (`:22-29`). Keep the setting lookup
**first**, so that an option genuinely named `append` still wins:

```js
get(_target, property) {
  if (typeof property !== "string") return undefined;
  const child = path === "" ? property : `${path}.${property}`;
  const lookup = op_configuration_get(id, child);
  if (lookup.found) return lookup.value;
  if (property === "append" && op_configuration_repeats(id, path)) {
    return () => options(op_configuration_append(id, path));
  }
  return options(child);
},
```

`op_configuration_repeats` runs only when a caller reads `.append`. Thus a usual
path costs nothing, and a path that does not repeat gives an ordinary child
proxy rather than a function that always throws.

Add `append(): Options` to the `Configuration` type in `plugins/plugin-api.ts`,
and declare the repeated groups in the `Options` type of each plugin that uses
one.

**One open question for the user.** Their example was
`args.configuration.content.graphics_packs.append()`, with no `options()` call.
This plan keeps `options()`, because six plugins call it today
(`cemu.ts:77`, `dolphin.ts:238`, `duckstation.ts:96`, `pcsx2.ts:86`,
`retroarch.ts:87`) and because `render`, `entries`, and `options` would
otherwise become section names that a plugin cannot use. Ask before you remove
it.

## 12. Step 8 — the Cemu plugin

1. Add `[bool_style.inverted]` to `plugins/cemu/config/cemu.toml`, and add the
   three targets of paragraph 3.
2. Add `plugins/cemu/config/sections/graphics-packs.toml`, and add its path to
   `plugins/cemu/plugin.toml`.
3. In `plugins/cemu/cemu.ts`, call `append` before `render()`, then restore the
   plain loop at `:91-96`. Delete `injectGraphicPacks` (`:142`) and `escapeXml`
   (`:157`).
4. `selectedPackEntries` in `plugins/cemu/graphics-packs.ts` already gives the
   full `filename` and the list of presets. Keep it unchanged. Only its consumer
   changes.

Cemu compares the entry against the path of the `rules.txt` file, relative to
the user data path, with forward slashes (`GraphicPack2.cpp:592-595`,
`precompiled.h:538-543`). `selectedPackEntries` already writes exactly that
string, thus the value does not change.

---

# Part B — a single-pass `onConfigure`

## 13. Why the loop exists, and what it costs

`#stabilize` (`crates/runtime/polymer-deno/js/03_configuration.js:100-115`)
calls `onConfigure` up to eight times, and `hydrate` calls it one time before
that. Thus one emulator candidate can make nine calls, and `describe` starts
every candidate together.

The loop exists for one reason. `withDefaults` runs in the **host**, after the
plugin gives its result. The host finds the options it has not seen before, puts
their defaults in, and must then ask the plugin again, because the layout of the
plugin can depend on those new values.

The cost is large:

- `run_worker` builds a new tokio runtime and a **new worker isolate** for each
  call (`crates/runtime/polymer-pluginhost/plugin_runtime/mod.rs:659-664`), and
  it loads the module graph again.
- `onConfigure` in Cemu calls `loadGraphicsPacks` (`plugins/cemu/cemu.ts:65`),
  which walks the tree and parses each `rules.txt` file. There are 401 of them
  in `plugins/cemu/bin/graphicsPacks`.
- A fresh isolate keeps no cache between the calls.

Thus one Cemu game can parse those 401 files nine times before the settings page
appears.

Two more defects:

- The comparison is `JSON.stringify` (`same()`). It is sensitive to the order of
  the keys, and it compares the titles and the descriptions, which never change.
- The failure gives `did not stabilize after 8 iterations`, and it names no
  option.

Cemu can genuinely oscillate. `evaluateCondition` reads `selectedVariables`,
which merges the values of the **selected** presets of every category
(`plugins/cemu/graphics-packs.ts:181-192`). Thus a selection in one category can
change which presets are available in a second category, and two categories that
condition each other can change each other without end.

## 14. What the old `depends` gave, and what returns

In the old design each resolver belonged to one option, and `depends` named the
option paths that the resolver reads. The union of every `depends` and every
`enabled_when` reference made a graph over the options, and that graph was a
property of the **document**. `materialize` sorted it one time and made one
pass. Three properties came from that order:

1. **One pass.** Each resolver operated one time, on settled inputs.
2. **A cycle was a load error** that named the cycle.
3. **The result did not depend on the number of the rounds.**

`depends` cannot return in the same form. It operated because each resolver was
attached to one option and the graph was declared. `onConfigure` is one function
that gives the whole layout at once, thus there is nothing to sort.

The change below gives back properties 1 and 3. It does not give back property
2, and nothing can: the host cannot see inside an opaque function. It moves that
decision to the plugin, which knows which category to hold constant.

## 15. Step 9 — the new contract

```ts
onConfigure(game, values) -> { schema, key, groups, values }
```

The plugin gives the layout **and** the settled values together. It knows its
own defaults and its own order of dependency, and it can iterate inside one
isolate at no cost.

File: `crates/runtime/polymer-deno/js/03_configuration.js`

- Delete `#stabilize` (`:100-115`), `withDefaults` (`:118-127`), and `same`
  (`:129-131`).
- `#configure` keeps the identity check (`:79-87`) and gives back both halves.
- `hydrate` becomes: read the stored values, call `#configure` one time, commit.
- `configure(values)` becomes: call `#configure` one time, commit if the revision
  still matches. Keep `#revision` and `settled()`; they guard against two
  overlapping calls from the UI, which is a different problem from convergence.

File: `crates/polymer-config/src/layout.rs`

`expand_layout` already validates the layout. Extend it to validate the values
in the same pass, because it holds both:

1. Every option in `groups` has a value in `values`.
2. `values` holds nothing that `groups` does not declare.
3. Each value agrees with the type, the range, and the domain of its option.

Add one error, `ConfigurationLayoutError::UnsettledValue { group, option }`, and
one, `UnknownValue { group, option }`.

File: `crates/runtime/polymer-pluginhost/plugin_runtime/mod.rs`

`configure` (`:336-392`) passes the whole result to `expand_layout` (`:389`).
The shape of the result changes, thus update the call and the test plugin at
`:1213`.

File: `plugins/cemu/cemu.ts`

`onConfigure` fills its own defaults before it gives its result. The pack layout
already computes a `fallback` for each category
(`plugins/cemu/graphics-packs.ts:88-90`); use it as the value when the user has
selected none. Where two categories condition each other, hold the earlier
category constant and state that rule in the commit message.

Update every other plugin that exports `onConfigure`.

**Caching is still worth doing, and it is not a replacement.** After this change
Cemu parses the 401 files one time for each call rather than nine times. Two
calls remain: one at `describe`, and one at each edit by the user.

---

## 16. Order of the work

Part A:

1. Step 1, and the parse check of paragraph 5.
2. Step 2.
3. Step 6, the type changes only. Repair each construction of `SectionRef`. The
   known place is the test `builds_nested_xml_and_escapes_text`
   (`ser/emitter/xml.rs:139-146`). Search for the others.
4. Step 4, the instance.
5. Step 5, the serializer.
6. Step 6, the XML emitter and the two guards.
7. Step 3, the rules and the errors.
8. Step 7, the host bridge.
9. Step 8, the Cemu plugin.

Part B, which does not depend on Part A:

10. Step 9, the contract, the host, and every plugin that exports
    `onConfigure`.

11. Ask the user about the tests of paragraph 17.

Run `cargo check -p polymer-config` after items 3 and 7. Run
`cargo test -p polymer-config` after item 7, and the workspace tests after
items 9 and 10. The test `every_plugin_package_parses`
(`toml/document.rs:471`) must stay green: it reads the new Cemu document from
item 9.

## 17. Proposed test coverage — **get approval first**

The user curates the test surface. Propose this list, and wait for the answer.
Do not add a test before the user approves it.

Part A:

1. A repeated section materializes no setting until `append` is called.
2. Two `append` calls give `…0.` and `…1.`, and the values stay separate.
3. A nested `append` puts the child below the correct parent.
4. `append` on a section that does not repeat gives `NotRepeatable`.
5. The XML emitter writes two `<Entry>` elements in the append order, each with
   its attribute and its `<Preset>` children.
6. A value that contains a quotation mark and an ampersand is escaped in an
   attribute.
7. `bool_style = "inverted"` with the value `true` gives `disabled="false"`.
8. A repeated target with no instance is not written at all.
9. An attribute on an INI target gives `AttributeUnsupportedFormat`.
10. An attribute on a document root gives `AttributeOnDocumentRoot`.
11. A `source` reference to a repeated option gives `UnknownSource`.
12. An option genuinely named `append` still reads as a value through the proxy.

Part B:

13. `onConfigure` is invoked exactly one time for one `describe`.
14. A layout with an option that has no value gives `UnsettledValue`.
15. A value that no group declares gives `UnknownValue`.
16. A value of the wrong type for its option is rejected.

Repair of a test that stops compiling is different, and it is necessary.

## 18. Update DESIGN.md

`crates/polymer-config/DESIGN.md` is the source of truth for this crate.

- §3.1: add `repeat` to the target.
- §3.2: add `repeat` to the section, and add `xml.attribute` beside
  `bool_style` and `int_style`.
- §3.5 and §9: mark the resolver domain and the `ResolverRuntime` trait as
  withdrawn. `onConfigure` replaced them. Record that `depends` gave a single
  pass and a load-time cycle error, that the single-pass contract of Part B
  gives the first back, and that the second now belongs to the plugin.
- §5.2: record that a repeated instance is keyed by an ordinal, and state why a
  stable key is not needed.
- §8.1: record the four rules of paragraph 7.
