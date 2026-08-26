import { Dialog } from "@ark-ui/solid";
import { onCleanup, onMount } from "solid-js";
import { useShell } from "../../app/ShellProvider";
import { ControllerAssignmentPanel } from "../../components/ControllerAssignmentPanel";
import "./ControllerRemapping.css";

export function ControllerRemapping() {
  const shell = useShell();
  const close = () => { shell.setModel("pauseOpen", true); shell.navigate("/game/play"); };
  onMount(() => {
    shell.model.launch?.deactivateInput();
    const keydown = (event: KeyboardEvent) => { if (event.key === "Escape") { event.preventDefault(); close(); } };
    window.addEventListener("keydown", keydown);
    onCleanup(() => window.removeEventListener("keydown", keydown));
  });
  return <Dialog.Root open modal={false} closeOnEscape={false} preventScroll={false}>
    <Dialog.Positioner class="horizon-remap-positioner"><Dialog.Content class="horizon-remap-panel horizon-input-scope"><Dialog.Title class="horizon-visually-hidden">Controller assignments</Dialog.Title><ControllerAssignmentPanel onClose={close} /></Dialog.Content></Dialog.Positioner>
  </Dialog.Root>;
}
