import { Dialog } from "@ark-ui/solid";
import { onCleanup, onMount } from "solid-js";
import { InputRemapping } from "../../../../chipotle/src/input-remapping";
import { useShell } from "../../app/ShellProvider";
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
    <Dialog.Positioner class="horizon-remap-positioner"><Dialog.Content class="horizon-remap-panel"><Dialog.Title>Controller mapping</Dialog.Title><Dialog.Description>Choose a control, then press or move its replacement.</Dialog.Description><div class="horizon-remap-frame"><InputRemapping onClose={close} /></div></Dialog.Content></Dialog.Positioner>
  </Dialog.Root>;
}
