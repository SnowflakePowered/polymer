import { Show, createSignal } from "solid-js";
import type { InputSourceSelector, ProjectionMode } from "@polymer-fe/frontend";
import { Button, DropdownField, Field, Modal } from "../../ui";
import { MappingEditor } from "../controllers/mapping/MappingEditor";
import { MappingGuide } from "../controllers/mapping/MappingGuide";
import "../controllers/mapping/mapping.css";

const INHERIT = "inherit";

export function PortMapping(props: { launchable: string; group: string; port: number; slot: number; layoutId: string }) {
  const controllers = window.Polymer.controllers;
  const layout = () => controllers.layouts.find(candidate => candidate.id === props.layoutId);
  const [open, setOpen] = createSignal(false);
  const device = () => controllers.players.find(player => player.index === props.slot)?.controller ?? undefined;
  const source = (): InputSourceSelector | undefined => { const controller = device(); return controller ? controller.id === "keyboardMouse" ? { kind: "keyboardMouse" } : { kind: "device", deviceId: controller.id } : undefined; };
  const editor = new MappingEditor(() => ({ kind: "game", launchable: props.launchable, group: props.group, port: props.port, source: source() }), layout, () => true);

  return <Show when={layout()}>{current => <>
    <DropdownField label="Button layout" value={editor.mapping()?.mode ?? INHERIT}
      options={[{ value: INHERIT, label: "Use controller default" }, { value: "name", label: "Match names" }, { value: "position", label: "Match positions" }]}
      onChange={value => editor.setMode(value === INHERIT ? null : value as ProjectionMode)} />
    <Field label="Buttons" description="Rebind buttons for this game and port.">
      <Show when={editor.customized()}><Button onClick={editor.reset}>Reset</Button></Show>
      <Button onClick={() => setOpen(true)}>Customize…</Button>
    </Field>
    <Modal open={open()} onClose={() => setOpen(false)} title="Customize buttons" class="vp-mapping-modal" buttons={<Button primary data-autofocus onClick={() => setOpen(false)}>Done</Button>}>
      <MappingGuide editor={editor} layout={current()} device={device()} />
    </Modal>
  </>}</Show>;
}
