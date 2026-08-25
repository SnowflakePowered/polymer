import { createEffect, onCleanup, onMount } from "solid-js";
import { useShell } from "../../app/ShellProvider";
import { PauseOverlay } from "./PauseOverlay";

export function PlayPage() {
  const shell = useShell();
  createEffect(() => {
    if (shell.model.pauseOpen) shell.model.launch?.deactivateInput();
    else shell.model.launch?.activateInput();
  });
  onMount(() => {
    const keydown = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.repeat || event.defaultPrevented) return;
      event.preventDefault();
      event.stopPropagation();
      shell.setModel("pauseOpen", (open: boolean) => !open);
    };
    window.addEventListener("keydown", keydown);
    onCleanup(() => { window.removeEventListener("keydown", keydown); shell.model.launch?.activateInput(); });
  });
  return <PauseOverlay />;
}
