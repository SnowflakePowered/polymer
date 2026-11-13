import { Show, createSignal } from "solid-js";
import type { InputSourceSelector, ProjectionMode } from "@polymer-fe/frontend";
import { Button, Dialog, Select, SettingRow } from "../../ui";
import { MappingEditor } from "../controllers/MappingEditor";
import { MappingGuide } from "../controllers/MappingGuide";
import "../controllers/MappingPage.css";

const INHERIT = "inherit";

export function PortMapping(props: { launchable: string; group: string; port: number; slot: number; layoutId: string }) {
  const controllers = window.Polymer.controllers;
  const layout = () => controllers.layouts.find(candidate => candidate.id === props.layoutId);
  const [open, setOpen] = createSignal(false);
  const device = () => controllers.players.find(player => player.index === props.slot)?.controller ?? undefined;
  const source = (): InputSourceSelector | undefined => { const controller = device(); return controller ? controller.id === "keyboardMouse" ? { kind: "keyboardMouse" } : { kind: "device", deviceId: controller.id } : undefined; };
  const editor = new MappingEditor(() => ({ kind: "game", launchable: props.launchable, group: props.group, port: props.port, source: source() }), layout, () => true);

  return <Show when={layout()}>{current => <SettingRow>
    <Select label="Button layout" value={editor.mapping()?.mode ?? INHERIT} items={[{ value: INHERIT, label: "Use controller default" }, { value: "name", label: "Match names" }, { value: "position", label: "Match positions" }]}
      onValueChange={value => value !== null && editor.setMode(value === INHERIT ? null : value as ProjectionMode)} />
    <div class="horizon-port-mapping-actions">
      <Button onClick={() => setOpen(true)}>Customize buttons…</Button>
      <Show when={editor.customized()}><Button onClick={editor.reset}>Reset</Button></Show>
    </div>
    <Dialog open={open()} onOpenChange={setOpen} title="Customize buttons" class="horizon-mapping-dialog" actions={<Button onClick={() => setOpen(false)}>Done</Button>}>
      <MappingGuide editor={editor} layout={current()} device={device()} />
    </Dialog>
  </SettingRow>}</Show>;
}
