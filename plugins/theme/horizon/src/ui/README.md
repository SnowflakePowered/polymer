# Horizon UI

The public entry point is `src/ui/index.ts`. Components use Solid signals and Ark behavior; they do not import Polymer services, models, or shell state. The host owns data, persistence, routing, notification lifetime and emulator pause/input leases.

## Development gallery

Run `npm run dev` in `plugins/theme/horizon`, then open `/gallery.html`. This standalone development entry uses local state and requires no Polymer API mocks. The native debug theme also exposes **UI gallery** in its rail. The gallery is excluded from production builds.

Press **F2** in the gallery to preview semantic controller input with the keyboard: arrows move, Enter/Space activate, Escape backs out, PageUp/PageDown represent L/R. This exercises the real UI routing and Ark controller adapters; it does not simulate hardware connection or repeat timing. F2 returns to ordinary keyboard editing.

The gallery includes every component family, grouped/disabled choices, loading/empty/error results, field validation, long labels, nested pickers, confirmations, context menus, side panels and draggable floating panels.

## Public inventory

| Family | Exports |
|---|---|
| Layout | Screen, ScreenHeader, NavigationRail, MasterDetail, Section, SettingRow, DetailList, Divider, ActionHints |
| Actions | Button, IconButton, ActionRow, ActionGroup |
| Fields | Field, TextField, NumberField, Toggle, Checkbox, RadioGroup, Slider |
| Choices | Select, MultiSelect, Combobox |
| Navigation | Tabs, CategoryNavigation, SelectableList, ListRow, TileGrid |
| Overlays | Dialog, DialogParts, ConfirmDialog, ActionDialog, ContextMenu, SidePanel, Popover, FloatingPanel, OverlayPortal |
| Feedback | Progress, Spinner, Toast, JobIndicator, EmptyState, ErrorState, Badge |
| Media | Icon, Avatar, CoverArt, Tile, EmptySlot, ButtonGlyph, ButtonChord, ControllerIllustration, ControllerSlot, PlayerBadge, ScrollingText |
| Interaction | NavigationProvider, NavigationController, useNavigation, useInputScope |

`DialogParts`, `Popover`, and `FloatingPanel` retain Ark compound APIs. `Dialog` is the convenience composition with a title, body and fixed action area. ActionDialog is a centered list of actions; ContextMenu uses actual menu semantics.

## Controlled choices

```tsx
const [system, setSystem] = createSignal<string | null>("wii");
const [visible, setVisible] = createSignal<string[]>(["wii"]);
const systems = [{ value: "wii", label: "Wii", group: "Nintendo" }];

<Select label="System" items={systems} value={system()} onValueChange={setSystem} />
<MultiSelect label="Visible systems" items={systems} value={visible()} onValueChange={setVisible} />
<Combobox label="Find system" items={systems} value={system()} onValueChange={setSystem} />
<Combobox multiple label="Find systems" items={systems} value={visible()} onValueChange={setVisible} />
```

ChoiceItem has stable string `value`, `label`, and optional `description`, `disabled`, and `group`. Choice fields accept `description`, `error`, `placeholder`, `disabled`, `readOnly`, `required`, `name`, `loading`, and `loadError`.

Single selection commits on activation. Highlighting never changes a committed value. Multiple selection snapshots values on opening and edits a draft through Ark. Done commits once; Cancel, Escape/B, outside dismissal and leaving the popup discard it. An external committed-value change cancels the open draft. Done/Cancel are siblings of the listbox, inside the popup boundary.

Combobox filters labels case-insensitively. Query text is separate from values and is cleared on dismissal. Selection survives filtering. There is no freeform creation or network fetching. The settings feature continues to use scalar controls only.

## Fields and feature adapters

All form controls require labels and associate descriptions/errors with the actual input. Field is available for custom readouts. Native hidden inputs retain Ark form behavior. Button supports `appearance="primary" | "secondary" | "danger"`, `disabled`, and `pending`.

Schema-specific OptionControl lives in `features/details`; VolumeSlider lives in `features/game`. Generic fields receive values and callbacks. Do not add configuration schema or storage behavior to the UI layer.

## Input ownership

Wrap consumers in NavigationProvider. Polymer's InputProvider translates keyboard and controller state into InputAction values carrying type, direction, origin and sourceId. Scopes register through useInputScope:

```tsx
useInputScope(() => ({
  root,
  navigation: true,
  hints: () => [{ action: "activate", label: "Open" }],
  handle: action => {
    if (action.type !== "activate") return false;
    openSelection();
    return true;
  },
}));
```

Return true only when consumed. Routing starts within the top overlay, then the focused control, its containing panes and screen. Overlay scopes block fallthrough. Ark keyboard handling and editable fields retain native behavior. Controller adapters use Ark public APIs. Only ordinary buttons/links receive generic activation.

Overlay registration owns initial/return focus. Disposal restores the trigger, or a valid control in its parent scope. Lists consume the vertical axis, tabs/sliders/numbers their adjustment axis, and geometric navigation connects panes. Footer prompts follow the active scope and keyboard/controller modality. Library selection stays separate per controller.

Directional input repeats after 350 ms and then every 90 ms. Stick activation/release thresholds are 12000/8000. Disconnect, scope changes, blur and disposal clear timers. Activation and Back do not repeat. PauseDecorator remains the only emulator pause/input lease owner.

## Design system and Penpot

Theming uses Panda CSS. `palette.ts` defines the light (Basic White) and dark (Basic Black) colors; `panda.config.ts` turns them into semantic tokens whose dark values apply under `[data-color-mode=dark]`. `ColorModeProvider` sets that attribute, and the host persists the choice from the Settings page. `tokens.css` maps the `hz-*` vocabulary onto the Panda variables. Component CSS lives in the `horizon` cascade layer, below Panda's layers, so `css()` styles override it. Blitz rasterizes inline SVG once, so `Icon` takes a `tone` and paints the palette color for the active mode instead of inheriting `currentColor`. Roboto is bundled under `assets/fonts` with its OFL license. Selection/action blue is independent from the turquoise focus outline. Animation, progress and overflow text honor reduced motion.

Penpot file: `c828d3cf-7d4e-8145-8008-9b925494739a`.
Component page: **Horizon · Components**, `fc632ade-d298-80b8-8008-ad89bcd8606b`.
The Light token set uses matching names; `hz-motion` is a numeric millisecond token and the text samples use the reference-size type scale. There are 52 named component families and 29 variant families. Existing Toast/JobIndicator components were organized into Running/Loading/Complete variants. Sixteen repeated screen rails/prompt bars now use shared instances. Original reference pages and cover artwork were preserved.

## Embedded runtime limits

The UI retains display-contents portals, closed-state visibility rules, transparent game surfaces and the debug-toolbar offset. It supplies local compatibility for missing select.options and tab-index defaults; `embeddedRuntime` marks code paths that differ from a browser. Where MutationObserver records cannot support Ark's focus trap, registered scopes provide the focus boundary and Tab containment.

Ark hidden inputs use `hz-hidden-input`: a contained, transparent 1px box with native focus and form semantics. Do not apply visible input sizing to these elements or rely on clipping alone to hide them. Toggle, checkbox and radio focus frames use Ark's `data-focus` attributes because Blitz does not propagate `:focus-within` like browsers. Component button layout is explicit to override Blitz's default inline-flex button styling. Focus frames use `:focus` because this Blitz revision never matches `:focus-visible`. The DOM bridge reflects the `hidden` property so Ark indicators and inactive content leave layout correctly. Checkbox marks use fixed-size SVGs, independent of font baselines.

The embedded DOM bridges scroll offsets, content/client dimensions, scrollTo/scrollBy and scrollIntoView to Blitz. Ark's public scroll callback reveals highlighted options inside their list; nested containers scroll inside-out, and focus respects preventScroll. Scroll events and geometry synchronize after native and programmatic movement. Nested scrollIntoView reveals immediately; explicit smooth scrollTo/scrollBy uses Blitz animation. Emulator JS–Rust interfaces are unchanged.

## Verification

Use `npm exec --no -- tsc -p tsconfig.app.json --noEmit` and `npm run build`. No plugin tests or mocks are added. See VALIDATION.md for completed checks and outstanding hardware/runtime verification.

SettingRow owns the focus frame for its complete option block. Descendant focus is tracked explicitly for Blitz; the native input or picker trigger still owns keyboard and accessibility behavior. Geometry-changing native resolves invalidate measurement caches so painted text uses the final box width.

Closed Select/MultiSelect triggers route arrow keys through the input scopes; Enter/Space opens the picker, where Ark handles option navigation and selection. Checkbox and toggle inputs allow directional navigation while retaining native activation. CategoryNavigation accepts `onEnter` for Enter/Right to enter its content pane. Game settings registers that pane separately so Up/Down stays among its controls and Left returns to the active category.
