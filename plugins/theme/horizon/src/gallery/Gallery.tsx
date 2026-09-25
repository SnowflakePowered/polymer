import exampleCover from "./cover.svg";
import { For, Show, createSignal, onCleanup, onMount } from "solid-js";
import { ActionDialog, ActionGroup, ActionHints, ActionRow, Avatar, Badge, Button, ButtonChord, ButtonGlyph, CategoryNavigation, Checkbox, Combobox, ConfirmDialog, ContextMenu, ControllerIllustration, ControllerSlot, CoverArt, DetailList, Dialog, Divider, EmptySlot, EmptyState, ErrorState, Field, FloatingPanel, Icon, IconButton, JobIndicator, MasterDetail, MultiSelect, NavigationRail, NumberField, OverlayPortal, PlayerBadge, Popover, Progress, RadioGroup, Screen, ScreenHeader, ScrollingText, Section, Select, SelectableList, SettingRow, SidePanel, Slider, Spinner, Tabs, TextField, Tile, TileGrid, Toast, Toggle, useNavigation, type Direction, type ActionName, type ChoiceItem } from "../ui";
import "./gallery.css";

const resolutions: ChoiceItem[] = [
  { value: "auto", label: "Automatic" }, { value: "720", label: "720p (HD)" }, { value: "1080", label: "1080p (Full HD)" }, { value: "1440", label: "1440p (WQHD)" }, { value: "2160", label: "2160p (4K)" },
];
const systems: ChoiceItem[] = [{ value: "nes", label: "Nintendo Entertainment System", group: "Nintendo" }, { value: "snes", label: "Super Nintendo", group: "Nintendo" }, { value: "n64", label: "Nintendo 64", group: "Nintendo" }, { value: "gc", label: "Nintendo GameCube", group: "Nintendo" }, { value: "wii", label: "Wii", group: "Nintendo" }, { value: "ps2", label: "PlayStation 2", group: "Sony" }, { value: "ps3", label: "PlayStation 3", group: "Sony", disabled: true }];
export function Gallery(props: { onBack?: () => void }) {
  const navigation = useNavigation();
  let externalChange: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(externalChange));
  const [controllerPreview, setControllerPreview] = createSignal(false);
  onMount(() => {
    const preview = (event: KeyboardEvent) => {
      if (event.key === "F2") { setControllerPreview(value => !value); event.preventDefault(); event.stopPropagation(); return; }
      if (!controllerPreview() || event.isComposing || event.ctrlKey || event.altKey || event.metaKey) return;
      const directions: Record<string, Direction> = { ArrowUp: "up", ArrowDown: "down", ArrowLeft: "left", ArrowRight: "right" };
      const actions: Record<string, ActionName> = { Enter: "activate", " ": "activate", Escape: "back", PageUp: "previous", PageDown: "next" };
      const type = directions[event.key] ? "move" : actions[event.key];
      if (!type) return;
      event.stopPropagation();
      if (event.repeat && type !== "move") { event.preventDefault(); return; }
      navigation.dispatch({ type, direction: directions[event.key], origin: "controller", sourceId: "gallery-preview", event });
    };
    document.addEventListener("keydown", preview, true);
    onCleanup(() => document.removeEventListener("keydown", preview, true));
  });
  const [resolution, setResolution] = createSignal<string | null>("auto");
  const [selected, setSelected] = createSignal<string[]>(["nes", "gc"]);
  const [search, setSearch] = createSignal<string | null>("n64");
  const [checked, setChecked] = createSignal(true), [text, setText] = createSignal("Living room"), [number, setNumber] = createSignal(3), [volume, setVolume] = createSignal(65);
  const [dialog, setDialog] = createSignal(false), [confirm, setConfirm] = createSignal(false), [side, setSide] = createSignal(false), [actions, setActions] = createSignal(false), [toast, setToast] = createSignal(true), [panel, setPanel] = createSignal(false);
  const [tab, setTab] = createSignal("one"), [category, setCategory] = createSignal("display"), [list, setList] = createSignal("one");
  const [longFocused, setLongFocused] = createSignal(false);
  const [status, setStatus] = createSignal("Ready");
  const items = [{ value: "one", label: "Super Mario 64", description: "Nintendo 64 · 24 MB" }, { value: "two", label: "Super Mario World", description: "Super NES · 14 MB" }, { value: "three", label: "A very long title that demonstrates scrolling text without changing the list layout", description: "Selected and focused states are independent" }];
  return <Screen class="hz-gallery" onAction={action => { if (action.type !== "back" || !props.onBack) return false; props.onBack(); return true; }} hints={[{ action: "move", label: "Move" }, { action: "activate", label: "Select" }]}>
    <ScreenHeader title="Horizon UI" description="Solid + Ark · component gallery"><Badge>{controllerPreview() ? "Controller preview · F2 to exit" : "Development · F2 previews controller input"}</Badge><Show when={props.onBack}><Button onClick={props.onBack}>Return to library</Button></Show></ScreenHeader>
    <div class="hz-gallery-scroll">
      <Section title="Choice controls" description="Automatic stays selected while focus moves. Multiple choices apply only with Done.">
        <SettingRow><Select label="TV Resolution" items={resolutions} value={resolution()} onValueChange={setResolution} /></SettingRow>
        <SettingRow><MultiSelect label="Visible systems" items={systems} value={selected()} onValueChange={setSelected} /></SettingRow>
        <SettingRow><Combobox label="Search system" items={systems} value={search()} onValueChange={setSearch} /></SettingRow>
        <SettingRow><Combobox multiple label="Search multiple systems" items={systems} value={selected()} onValueChange={setSelected} /></SettingRow>
        <p class="hz-description" aria-live="polite">Committed systems: {selected().join(", ") || "none"}</p>
        <ActionGroup><Button onClick={() => setSelected(["wii"])}>External selection → Wii</Button><Button onClick={() => setDialog(true)}>Open nested picker dialog</Button><Button onClick={() => { clearTimeout(externalChange); externalChange = setTimeout(() => setSelected(["wii"]), 3000); }}>Replace selection in 3 seconds</Button></ActionGroup>
      </Section>
      <Section title="Fields and state variants">
        <SettingRow><Toggle label="HDR Output" description="Enable HDR on supported displays." checked={checked()} onCheckedChange={setChecked} /></SettingRow>
        <SettingRow><Checkbox label="Include updates" checked={checked()} onCheckedChange={value => setChecked(value === true)} /></SettingRow>
        <SettingRow><Checkbox label="Some items selected" checked="indeterminate" onCheckedChange={() => {}} /></SettingRow>
        <SettingRow><TextField label="Name" value={text()} onValueChange={setText} /></SettingRow>
        <SettingRow><NumberField label="Players" value={number()} min={1} max={8} onValueChange={setNumber} /></SettingRow>
        <SettingRow><Slider label="Volume" value={volume()} onValueChange={setVolume} /></SettingRow>
        <SettingRow><RadioGroup label="Output" value={resolution()} items={resolutions.slice(0, 3)} onValueChange={setResolution} /></SettingRow>
        <SettingRow><TextField label="Invalid field" value="" onValueChange={() => {}} error="Enter a name." required /></SettingRow>
        <SettingRow><Select label="Disabled" disabled items={resolutions} value="auto" onValueChange={() => {}} /></SettingRow>
        <SettingRow><Select label="Read only" readOnly items={resolutions} value="1080" onValueChange={() => {}} /></SettingRow>
        <SettingRow><Select label="Loading choices" loading items={[]} value={null} onValueChange={() => {}} /></SettingRow>
        <SettingRow><Select label="Empty choices" items={[]} value={null} onValueChange={() => {}} /></SettingRow>
        <SettingRow><Select label="Failed choices" loadError="Could not load options." items={[]} value={null} onValueChange={() => {}} /></SettingRow>
        <Field label="Custom field" description="Fields compose with custom content."><Badge>Readout</Badge></Field>
      </Section>
      <Section title="Actions and overlays"><ActionGroup>
        <Button appearance="primary" onClick={() => setStatus("Primary action")}>Primary</Button><Button>Secondary</Button><Button appearance="danger" onClick={() => setConfirm(true)}>Delete…</Button><Button disabled>Disabled</Button><Button pending>Working</Button><IconButton icon="settings" label="Settings" />
        <Button onClick={() => setActions(true)}>Actions…</Button><Button onClick={() => setSide(true)}>Side panel</Button><Button onClick={() => setPanel(true)}>Floating panel</Button>
        <Popover.Root><Popover.Trigger class="hz-button">Popover</Popover.Trigger><OverlayPortal><Popover.Positioner class="hz-picker-positioner"><Popover.Content><Popover.Title>Audio</Popover.Title><Slider label="Volume" value={volume()} onValueChange={setVolume} /><Popover.CloseTrigger class="hz-button">Close</Popover.CloseTrigger></Popover.Content></Popover.Positioner></OverlayPortal></Popover.Root>
      </ActionGroup><ActionRow description="A secondary description" onClick={() => setStatus("Action row")}>Action row</ActionRow><ContextMenu label="Context menu" actions={[{ label: "Open", run: () => setStatus("Open") }, { label: "Unavailable", disabled: true, run() {} }, { label: "Delete", danger: true, run: () => setConfirm(true) }]}><span>Right-click here</span></ContextMenu><p role="status">{status()}</p></Section>
      <Section title="Navigation and layout"><Tabs value={tab()} onValueChange={setTab} items={[{ value: "one", label: "Profile one", content: <p>Profile one content</p> }, { value: "two", label: "Profile two", content: <p>Profile two content</p> }]} />
        <div style={{ height: "22rem" }}><MasterDetail navigation={<CategoryNavigation label="Settings categories" value={category()} onValueChange={setCategory} items={[{ value: "display", label: "Display" }, { value: "audio", label: "Audio" }]} />} detail={<DetailList items={[{ label: "Version", value: "1" }, { label: "Installed size", value: "24 MB" }]} />}><SelectableList label="Installed titles" value={list()} onValueChange={setList} items={items} /></MasterDetail></div>
        <NavigationRail aria-label="Example rail"><IconButton icon="library" label="Library" /><IconButton icon="saves" label="Saves" /></NavigationRail>
      </Section>
      <Section title="Media and controllers"><TileGrid><Tile selected><div style={{ padding: "1rem" }}><Icon name="library" /><p>Selected tile</p></div></Tile><Tile aria-label="Example cover"><CoverArt src={exampleCover} alt="Example cover" /></Tile><EmptySlot label="Empty slot" /><ControllerSlot player={0} connected name="Pro Controller"><ControllerIllustration kind="pro" /></ControllerSlot></TileGrid>
        <ActionGroup><Avatar label="Profile" /><For each={["a", "b", "x", "y", "l", "r", "plus", "minus", "home", "dpad"] as const}>{button => <ButtonGlyph button={button} />}</For><ButtonChord buttons={["l", "r"]} /><PlayerBadge player={1} /><ControllerIllustration kind="left" /><ControllerIllustration kind="right" /><ControllerIllustration kind="console" /></ActionGroup>
        <Button style={{ width: "14rem" }} onFocus={() => setLongFocused(true)} onBlur={() => setLongFocused(false)}><ScrollingText active={longFocused()}>A focused long title scrolls when it exceeds the available space</ScrollingText></Button>
      </Section>
      <Section title="Feedback"><Progress label="Installing" completed={62} total={100} /><p>62% complete</p><Progress label="Loading" /><Spinner /><Badge>Installed</Badge><Badge danger>Failed</Badge><Divider />
        <Show when={toast()} fallback={<Button onClick={() => setToast(true)}>Show toast</Button>}><Toast title="Installing title" message="62% complete" onDismiss={() => setToast(false)}><Progress label="Installing title" completed={62} total={100} /></Toast></Show>
        <JobIndicator title="Scraping covers" message="Loading artwork" completed={3} total={10} onOpen={() => setToast(true)} /><EmptyState title="No titles installed" description="Install a title to get started."><Button>Install titles</Button></EmptyState><ErrorState message="The operation failed. Try again."><Button>Retry</Button></ErrorState>
      </Section>
    </div><ActionHints />
    <Dialog open={dialog()} onOpenChange={setDialog} title="Display options" description="Open a picker, then press Escape once to close only that picker." actions={<Button onClick={() => setDialog(false)}>Close dialog</Button>}><SettingRow><Select label="Resolution in dialog" items={resolutions} value={resolution()} onValueChange={setResolution} /></SettingRow><SettingRow><MultiSelect label="Systems in dialog" items={systems} value={selected()} onValueChange={setSelected} /></SettingRow></Dialog>
    <ConfirmDialog open={confirm()} title="Delete selection?" description="This gallery action only changes the status message." confirmLabel="Delete" onClose={() => setConfirm(false)} onConfirm={() => { setStatus("Deleted gallery selection"); setConfirm(false); }} />
    <ActionDialog open={actions()} title="Title actions" description="Nintendo 64" onClose={() => setActions(false)} actions={[{ label: "Continue playing", run: () => setStatus("Continue") }, { label: "Backups…", run: () => setStatus("Backups") }, { label: "Delete…", danger: true, run: () => setConfirm(true) }]} />
    <SidePanel open={side()} onOpenChange={setSide} title="Side panel" actions={<Button onClick={() => setSide(false)}>Close</Button>}><p>Reusable slide-out content.</p></SidePanel>
    <FloatingPanel.Root open={panel()} onOpenChange={details => setPanel(details.open)} defaultPosition={{ x: 120, y: 120 }} defaultSize={{ width: 360, height: 240 }}><FloatingPanel.Positioner style={{ "z-index": 140 }}><FloatingPanel.Content><FloatingPanel.Header><FloatingPanel.DragTrigger>Floating panel</FloatingPanel.DragTrigger><FloatingPanel.CloseTrigger>Close</FloatingPanel.CloseTrigger></FloatingPanel.Header><FloatingPanel.Body>Drag and resize this panel.</FloatingPanel.Body><FloatingPanel.ResizeTrigger axis="se">↘</FloatingPanel.ResizeTrigger></FloatingPanel.Content></FloatingPanel.Positioner></FloatingPanel.Root>
  </Screen>;
}
