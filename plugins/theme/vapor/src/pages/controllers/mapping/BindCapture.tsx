import { Show, createEffect, onCleanup } from "solid-js";
import type { InputState, ManagedControllerDevice } from "@polymer-fe/frontend";
import { Button, Modal } from "../../../ui";
import { keyGlyph } from "./keyGlyphs";
import type { PadFamily } from "./padGlyphs";
import { SourceChip } from "./SourceChip";

const THRESHOLD = 0.6;
const IDLE_MS = 10000;
const ARM_MS = 150;

function padSource(state: InputState, pad: { buttons: readonly string[] }) {
  const bit = pad.buttons.findIndex((_, index) => (state.buttons >>> index) & 1);
  if (bit >= 0) return pad.buttons[bit];
  const axes: readonly [readonly [number, number], string][] = [[state.leftStick, "left"], [state.rightStick, "right"]];
  for (const [[x, y], side] of axes) {
    if (Math.abs(x) > THRESHOLD) return `analog.stick.${side}.x${x > 0 ? "+" : "-"}`;
    if (Math.abs(y) > THRESHOLD) return `analog.stick.${side}.y${y > 0 ? "+" : "-"}`;
  }
  if (state.leftTrigger > THRESHOLD) return "trigger.left";
  if (state.rightTrigger > THRESHOLD) return "trigger.right";
  return undefined;
}

export function BindCapture(props: { open: boolean; target: string; label: string; glyph?: string; current: string; family: PadFamily; device?: ManagedControllerDevice; onCapture(source: string): void; onClose(): void }) {
  const controllers = window.Polymer.controllers;
  const player = () => props.device ? controllers.players.find(candidate => candidate.controller?.id === props.device!.id)?.index : undefined;

  createEffect(() => {
    props.target;
    if (!props.open) return;
    const armedAt = performance.now() + ARM_MS;
    const idle = setTimeout(() => props.onClose(), IDLE_MS);
    onCleanup(() => clearTimeout(idle));
    if (!props.device) {
      const key = (event: KeyboardEvent) => {
        event.preventDefault();
        event.stopImmediatePropagation();
        if (event.repeat || performance.now() < armedAt) return;
        if (event.code === "Escape") props.onClose();
        else props.onCapture(`key.${event.code}`);
      };
      const mouse = (event: MouseEvent) => {
        if ((event.target as Element | null)?.closest?.(".vp-modal-buttons") || performance.now() < armedAt) return;
        event.preventDefault();
        event.stopImmediatePropagation();
        props.onCapture(`mouse.button.${event.button}`);
      };
      window.addEventListener("keydown", key, true);
      window.addEventListener("mousedown", mouse, true);
      onCleanup(() => { window.removeEventListener("keydown", key, true); window.removeEventListener("mousedown", mouse, true); });
      return;
    }
    let released = false;
    const poll = setInterval(() => {
      const index = player();
      const state = index === undefined ? null : window.Polymer.input.get(index);
      if (!state) return;
      const source = padSource(state, controllers.pad);
      if (!source) released = true;
      else if (released) props.onCapture(source);
    }, 30);
    onCleanup(() => clearInterval(poll));
  });

  const description = () => props.device
    ? player() === undefined ? `${props.device.name} is not assigned to a player. Connect it first.` : `Press a button or move a stick on ${props.device.name}.`
    : "Press a key or mouse button.";

  return <Modal open={props.open} onClose={props.onClose} title={<>Rebind <Show when={props.glyph}>{src => <img class="vp-glyph vp-capture-target" src={src()} alt="" />}</Show> {props.label}</>} description={description()}
    onAction={() => true}
    buttons={<><Button onClick={() => props.onCapture("none")}>Clear</Button><Button data-autofocus onClick={props.onClose}>Cancel</Button></>}>
    <p class="vp-capture-current">Currently <span class="vp-callout vp-capture-chip"><SourceChip family={props.family} sources={[props.current]} /></span></p>
    <Show when={!props.device}><p class="vp-capture-cancel">Press <img class="vp-glyph" src={keyGlyph("key.Escape")} alt="Esc" /> to cancel.</p></Show>
  </Modal>;
}
