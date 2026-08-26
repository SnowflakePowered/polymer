import { Popover } from "@ark-ui/solid";
import { For, Show, createSignal, onCleanup, onMount } from "solid-js";
import { ControllerAssignmentPanel } from "./ControllerAssignmentPanel";
import { Icon } from "./Icon";
import "./ControllerStatus.css";

export function ControllerStatus() {
  const [revision, setRevision] = createSignal(0);
  const [open, setOpen] = createSignal(false);
  const changed = () => setRevision((value) => value + 1);
  onMount(() => {
    window.Snowglass.input.addEventListener("inputstatechange", changed);
    window.Snowglass.input.addEventListener("configurationchange", changed);
    onCleanup(() => {
      window.Snowglass.input.removeEventListener("inputstatechange", changed);
      window.Snowglass.input.removeEventListener("configurationchange", changed);
    });
  });
  const controllerCount = () => (revision(), window.Snowglass.input.controllers.length);
  const keyboardAssigned = () => (revision(), window.Snowglass.input.assignments.some((assignment) => assignment.source.kind === "keyboardMouse"));
  return <Popover.Root open={open()} onOpenChange={(details: any) => setOpen(details.open)} positioning={{ placement: "right-end", gutter: 16 }}>
    <Popover.Trigger class="horizon-controller-status" aria-label="Controller assignments">
      <Show when={controllerCount() > 0} fallback={<Icon name="keyboard" />}>
        <span class="horizon-connected-squares"><For each={Array.from({ length: controllerCount() })}>{() => <i />}</For></span>
      </Show>
      <Show when={keyboardAssigned()}><i class="horizon-keyboard-square" /></Show>
    </Popover.Trigger>
    <Popover.Positioner class="horizon-controller-status-positioner">
      <Popover.Content class="horizon-controller-status-popover horizon-input-scope">
        <Popover.Arrow><Popover.ArrowTip /></Popover.Arrow>
        <ControllerAssignmentPanel onClose={() => setOpen(false)} />
      </Popover.Content>
    </Popover.Positioner>
  </Popover.Root>;
}
