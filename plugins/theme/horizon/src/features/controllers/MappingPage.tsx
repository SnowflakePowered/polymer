import { Show, createEffect, createMemo, createSignal, on } from "solid-js";
import type { InputSourceSelector, ProjectionMode } from "@polymer-fe/frontend";
import { ActionRow, Button, Icon, RadioGroup, Screen, ScreenHeader, CategoryNavigation } from "../../ui";
import { AppSidebar } from "../../components/AppSidebar";
import { useShell } from "../../app/shell-context";
import { OutlinePreview } from "./ButtonGuide";
import { LayoutPicker } from "./LayoutPicker";
import { MappingEditor } from "./MappingEditor";
import { MappingGuide } from "./MappingGuide";
import { outlines } from "./outlines";
import "./MappingPage.css";

const KEYBOARD = "keyboardMouse";

const selectorOf = (value: string): InputSourceSelector => value === KEYBOARD ? { kind: "keyboardMouse" } : { kind: "device", deviceId: value };

export function MappingPage() {
  const shell = useShell();
  const controllers = window.Polymer.controllers;
  const layouts = controllers.layouts;
  const [source, setSource] = createSignal(KEYBOARD);
  const [layoutId, setLayoutId] = createSignal(layouts.find(layout => layout.id in outlines)?.id ?? layouts[0]?.id ?? "");
  const [editing, setEditing] = createSignal(false);
  createEffect(on(editing, () => queueMicrotask(() => document.querySelector<HTMLElement>(".horizon-mapping-screen [data-input-default]")?.focus()), { defer: true }));
  const layout = createMemo(() => layouts.find(candidate => candidate.id === layoutId()));
  const device = () => controllers.devices.find(candidate => candidate.id === source());
  let overview!: HTMLElement;
  const editor = new MappingEditor(() => ({ kind: "source", source: selectorOf(source()) }), layout, () => source() !== KEYBOARD);
  const sources = () => [{ value: KEYBOARD, label: "Keyboard & Mouse", description: "Keyboard and mouse" }, ...controllers.devices.map(candidate => ({ value: candidate.id, label: candidate.name, description: candidate.connected ? "Connected" : "Not connected" }))];

  return <div class="horizon-mapping-shell"><AppSidebar /><Screen class="horizon-mapping-screen" hints={[{ action: "back", label: "Back" }, { action: "activate", label: "OK" }]}
    onAction={action => {
      if (action.type !== "back") return false;
      if (editing()) setEditing(false);
      else shell.navigate("/controllers");
      return true;
    }}>
    <ScreenHeader title={editing() ? "Change Button Mapping" : "Button Mapping"} icon={<Icon name="gamepad" class="horizon-mapping-icon" strokeWidth={1.4} />} />
    <Show when={!editing()} fallback={<div class="horizon-mapping-editing">
      <Show when={layout()}>{current => <MappingGuide editor={editor} layout={current()} device={device()} />}</Show>
      <div class="horizon-mapping-editing-actions">
        <Show when={editor.customized()}><Button onClick={editor.reset}>Reset to default</Button></Show>
        <Button data-input-default onClick={() => setEditing(false)}>Done</Button>
      </div>
    </div>}>
      <div class="horizon-mapping-layout">
        <nav class="horizon-mapping-sources" aria-label="Controllers">
          <CategoryNavigation label="Controllers" value={source()} items={sources()} onValueChange={setSource} onEnter={() => overview.querySelector<HTMLElement>("button")?.focus()} />
        </nav>
        <section ref={overview} class="horizon-mapping-overview">
          <OutlinePreview layoutId={layoutId()} />
          <div class="horizon-mapping-actions">
            <ActionRow data-input-default onClick={() => setEditing(true)}>Change</ActionRow>
            <ActionRow disabled={!editor.customized()} onClick={editor.reset}>Reset</ActionRow>
          </div>
          <LayoutPicker layouts={layouts} value={layoutId()} onValueChange={setLayoutId} />
          <div class="horizon-mapping-controls">
            <Show when={source() !== KEYBOARD}>
              <RadioGroup label="Button layout" value={editor.mapping()?.mode ?? null} items={[{ value: "name", label: "Match names" }, { value: "position", label: "Match positions" }]} onValueChange={mode => editor.setMode(mode as ProjectionMode)} />
            </Show>
          </div>
        </section>
      </div>
    </Show>
  </Screen></div>;
}
