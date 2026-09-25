# Horizon UI validation — 2026-09-22

This records observed checks, not a claim that every completion criterion has passed.

## Blitz rendering follow-up — 2026-09-23

- Hidden checkbox, switch, radio, slider and select inputs now have explicit 1px transparent styling, isolated from visible input dimensions. Native gallery DOM measurements confirmed the 1×1 boxes and containment within their controls.
- Native switch focus sets Ark's `data-focus` on the visible control; shared focus styling now uses that state. List focus uses its existing Solid focus state instead of `:focus-within`.
- Navigation buttons, tiles and job indicators have explicit display/alignment. Action labels and prompt pairs do not wrap internally. The settings footer can wrap complete controls and has one divider.
- The gallery uses the available parent height. Native measurement at 1770×842 confirmed an 810px gallery (32px debug toolbar reserved) with its footer inside that height.
- Chromium checks confirmed hidden-input opacity/dimensions and checkbox label activation. Full pixel-by-pixel native screenshots have not been compared; the native checks here use the running Blitz DOM geometry and state.

## Text layout cache and row focus follow-up — 2026-09-23

- Reproduced the reported first-render corruption with Persona 4. Temporary native tracing showed a correct 886×19 header box, but Parley retained 37 min-content lines (roughly 14px wide and 701px tall). The earlier box-only check did not detect this.
- Geometry-changing and asset-driven resolves now invalidate Blitz's layout measurement caches before resolving. With the same native opening/focus/category sequence, the title remained one line (373×19 text) in every traced frame. Temporary tracing was removed afterward; no dependency versions changed.
- SettingRow tracks descendant focus without relying on unsupported `:focus-within`, draws one frame around the label, value and description, and suppresses the inner control outline. Native Port 1 focus marked the complete 742×77 row and left other rows unmarked. Chromium screenshot verified the full row frame with no inner frame.
- Native build, Horizon TypeScript check and production build passed.

## Return navigation, focus and checkbox follow-up — 2026-09-23

- Gallery Return to library uses Solid router navigation, rather than mutating memory history directly. A fresh native app returned from the gallery to game tiles after clicking it.
- Shared and feature focus styles now use `:focus`: this Blitz revision hardcodes `:focus-visible` matching to false. Sidebar focus has its own turquoise frame. Library per-controller selections use player-colored outlines instead of coloring a zero-width border.
- The DOM bridge now reflects `element.hidden`. Before the fix, both Ark checkbox indicators occupied layout even when their JavaScript hidden property was true. Native measurement now shows the inactive indicator at 0×0 and the active 18×18 SVG centered with 3px clearance inside the 24×24 box. Checked and indeterminate states were both inspected.
- Chromium gallery screenshot confirmed centered check/minus indicators and the keyboard focus frame on the mixed checkbox.
- Settings header uses a nonshrinking title and gives the metadata the remaining width. The first sampled native layout at 1280×720 measured the title at 210×39 and metadata at 886×19; neither wrapped.
- Native build, Horizon TypeScript check and production build passed. Physical gamepad and native pixel-level focus verification remain outstanding.

## Native scrolling follow-up — 2026-09-23

- `cargo check -p polymer-gui`, `cargo build -p polymer-gui`, the Horizon TypeScript app check and production build passed. Existing unrelated Rust warnings remain.
- Live Blitz gallery at 1280×720: scrollTop/scrollBy moved to 500/650px; an oversized request clamped to the 3379px maximum. The container stayed at y=100 while its child geometry moved by the scroll offset.
- Multiselect End skipped disabled PlayStation 3 and revealed PlayStation 2 by scrolling its list 68px, with the gallery and window unchanged. Home retained the first option in view; Escape preserved committed values and restored trigger focus.
- Offscreen focus revealed its control; focus({ preventScroll: true }) preserved the 500px gallery offset.
- A temporary live DOM fixture verified horizontal and vertical reveal through two nested containers (inner 214px/324px, outer 218px). container: nearest moved only the inner container. The fixture was removed afterward.
- Explicit smooth scrolling delivered intermediate scroll positions with matching child geometry, ending at 500px. Scroll events run after synchronized layout rather than accumulating until the next input.
- Nested scrollIntoView uses immediate movement; smooth animation is supported by scrollTo/scrollBy. Physical wheel, scrollbar dragging and controller hardware were not exercised in this follow-up.

## Passed

- Existing TypeScript app check and production `npm run build`.
- Public UI module has no Polymer types, `useShell`, or `window.Polymer` imports/access. Dependency versions, JS–Rust interfaces and ARCHITECTURE.md were unchanged.
- Browser gallery inspected at 1280×720, 1443×812 and 1920×1080. Popup height respects Ark's available space, with Done/Cancel reachable at 720p.
- Automatic remains selected while End moves the turquoise highlight to 2160p. Escape preserves Automatic and restores trigger focus.
- Multiselect edits remain drafts until Done. Done commits; nested Escape cancels without closing the containing dialog. Done/Cancel sit outside listbox semantics.
- Searchable single selection commits and clears search. Searchable multiple selection retains prior values when adding a filtered Wii result.
- Timed external replacement while a multiselect is open closes it and replaces the committed values; the draft cannot overwrite the update.
- Gallery controller preview opens a picker, toggles choices, pages past disabled entries, moves into Done and back into the list, then commits and restores focus. Switch glyph prompts follow controller modality.
- Text editing retains caret navigation, spaces, Enter behavior and input focus.
- Native `polymer-gui --theme horizon --debug` inspected through its debug protocol: library, settings categories, emulator selection, controller topology, storage, pause/audio and controller reorder all mounted.
- Native nested emulator picker → settings and audio popover → pause each close one layer on Escape and restore the corresponding trigger.
- Native focused-game keyboard Details opens that game, including after focus changes independently of the persistent selection.
- Native disposable Wii NAND created through the migrated dialog. The selector showed both existing and disposable Wii volumes. Confirmation deletion removed only the new volume. The original three volumes and null Wii selection were verified afterward.
- Penpot inventory checked: all 52 required component names present, 29 variant families, 25 tokens, six original notification components organized into variants, 16 repeated screen elements replaced with instances. Select and notification exports inspected visually.
- UI subscriptions, scope registrations, repeat timers, gallery timeout, observers and queued picker work have disposal cleanup. Repeated browser HMR updates and reloads exercised these paths.

## Remaining limitations / unverified checks

- **Full launch/pause/resume verification was blocked by the emulator environment.** The Dolphin test launch reported ready with zero tracked surfaces. Its pause/resume callback failed with `emulator action has no active target surface`. PauseDecorator and tracked-surface ownership were preserved. The validation launch was closed.
- No physical controllers were connected; Bluetooth initialization reported no dedicated adapter. Actual held-direction timing, hysteresis, disconnect behavior, multi-controller independence and pairing require hardware verification. The gallery preview does not certify these.
- Tested native game configurations returned no dynamic scalar settings groups. Persistence and dynamic refresh callbacks remain in GameDetailsPage, but a real scalar-edit round trip was not exercised.
- Storage artifact installation, bootstrap download, recovery and uninstall were not exercised against real contents. Their existing operations and safeguards are retained. Only the newly created empty validation NAND was deleted.
- Reduced-motion rules and IME composition guards were reviewed in code; OS-level reduced motion and IME composition were not exercised.
- Native pixel-level validation at all three viewport sizes, full cover zoom/scraping workflows, notification expiry/reopening under live jobs, and remount behavior during a working multi-surface game remain to be verified.

The component implementation and migrations are available for review, but the plan's complete native acceptance checklist is not yet satisfied.

## Submenu keyboard navigation follow-up — 2026-09-23

- Closed Ark Select triggers were consuming arrow keys before screen navigation could move focus. Their arrows now navigate through registered scopes; open pickers retain Ark keyboard handling.
- Verified through the native debug protocol on a fresh app mount: Controller topology → Enter → Port 1 → Down → Port 2 → Down → Port 3 → Up → Port 2 → Left → Controller topology. Right also enters the content pane.
- Verified native emulator picker: Enter opens, Down highlights Retroarch-Lrps2 while Pcsx2 remains selected, Up returns the highlight, and Enter commits Pcsx2 and restores the Emulator trigger without closing settings.
- Chromium gallery: Down moves from the closed TV Resolution trigger to Visible systems; Enter opens the multiselect, Down/Space toggles Super Nintendo, and Escape discards the draft and restores the trigger with its original values.
- TypeScript app check and production build pass. No plugin tests or mocks added; physical gamepad input was not exercised.
