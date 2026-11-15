import { Show, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import type { InputSourceSelector, ProjectionMode } from "@polymer-fe/frontend";
import { useShell } from "../../app/shell";
import { Button, DropdownField, Page, PagedSettings } from "../../ui";
import { OutlinePreview, uiScale } from "./mapping/ButtonGuide";
import { LayoutPicker } from "./mapping/LayoutPicker";
import { MappingEditor } from "./mapping/MappingEditor";
import { MappingGuide } from "./mapping/MappingGuide";
import { outlines } from "./mapping/outlines";
import "./mapping/mapping.css";

const KEYBOARD = "keyboardMouse";

const selectorOf = (value: string): InputSourceSelector => value === KEYBOARD ? { kind: "keyboardMouse" } : { kind: "device", deviceId: value };

export function MappingPage() {
  const shell = useShell();
  const controllers = window.Polymer.controllers;
  const layouts = controllers.layouts;
  const [source, setSource] = createSignal(KEYBOARD);
  const [layoutId, setLayoutId] = createSignal(layouts.find(layout => layout.id in outlines)?.id ?? layouts[0]?.id ?? "");
  const [editing, setEditing] = createSignal(false);
  const [viewport, setViewport] = createSignal(window.innerHeight);
  onMount(() => {
    const resize = () => setViewport(window.innerHeight);
    window.addEventListener("resize", resize);
    onCleanup(() => window.removeEventListener("resize", resize));
  });
  const layout = createMemo(() => layouts.find(candidate => candidate.id === layoutId()));
  const device = () => controllers.devices.find(candidate => candidate.id === source());
  const editor = new MappingEditor(() => ({ kind: "source", source: selectorOf(source()) }), layout, () => source() !== KEYBOARD);
  const pages = () => [{ id: KEYBOARD, title: "Keyboard & Mouse" }, ...controllers.devices.map(candidate => ({ id: candidate.id, title: candidate.name }))].map(item => ({
    ...item, icon: "controller" as const, content: () => <div class="mc-mapping-overview">
      <OutlinePreview layoutId={layoutId()} />
      <div class="mc-mapping-actions">
        <Button primary data-autofocus onClick={() => setEditing(true)}>Change</Button>
        <Button disabled={!editor.customized()} onClick={editor.reset}>Reset</Button>
      </div>
      <Show when={source() !== KEYBOARD}>
        <DropdownField label="Button layout" value={editor.mapping()?.mode ?? null} options={[{ value: "name", label: "Match names" }, { value: "position", label: "Match positions" }]} onChange={mode => editor.setMode(mode as ProjectionMode)} />
      </Show>
      <LayoutPicker layouts={layouts} value={layoutId()} onValueChange={setLayoutId} />
    </div>,
  }));

  return <Page class="mc-mapping" focusWhen={editing} hints={[{ action: "activate", label: "Select" }, { action: "back", label: "Back" }]} onAction={action => {
    if (action.type !== "back") return false;
    if (editing()) setEditing(false);
    else shell.navigate("/settings");
    return true;
  }}>
    <Show when={!editing()} fallback={<div class="mc-mapping-editing" style={{ "--k": uiScale(viewport()) }}>
      <h1 class="mc-mapping-heading">Change Button Mapping</h1>
      <p class="mc-mapping-subtitle">{pages().find(page => page.id === source())?.title} — {layout()?.consoles.join(", ")} — {layout()?.name === layout()?.id ? "Layout" : layout()?.name}</p>
      <Show when={layout()}>{current => <MappingGuide editor={editor} layout={current()} device={device()} />}</Show>
      <div class="mc-mapping-editing-actions">
        <Show when={editor.customized()}><Button onClick={editor.reset}>Reset to default</Button></Show>
        <Button primary data-autofocus onClick={() => setEditing(false)}>Done</Button>
      </div>
    </div>}>
      <PagedSettings title="Button Mapping" active={source()} onChange={setSource} pages={pages()} />
    </Show>
  </Page>;
}
