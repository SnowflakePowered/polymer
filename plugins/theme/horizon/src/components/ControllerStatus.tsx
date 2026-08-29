import { For, Show, createSignal, onCleanup, onMount } from "solid-js";
import { ControllerReorder } from "../features/game/ControllerReorder";
import { Icon } from "./Icon";
import "./ControllerStatus.css";

export function ControllerStatus() {
  const [revision, setRevision] = createSignal(0);
  const [open, setOpen] = createSignal(false);
  const changed = () => setRevision((value) => value + 1);
  onMount(() => {
    window.Snowglass.controllers.addEventListener("statechange", changed);
    onCleanup(() => {
      window.Snowglass.controllers.removeEventListener("statechange", changed);
    });
  });
  const controllerCount = () => (revision(), window.Snowglass.controllers.connected.length);
  return <>
    <button type="button" class="horizon-controller-status" aria-label="Change controller grip or order" aria-expanded={open()} onClick={() => setOpen(true)}>
      <Show when={controllerCount() > 0} fallback={<Icon name="keyboard" />}>
        <span class="horizon-connected-squares"><For each={Array.from({ length: controllerCount() })}>{() => <i />}</For></span>
      </Show>
    </button>
    <Show when={open()}><ControllerReorder onClose={() => setOpen(false)} /></Show>
  </>;
}
