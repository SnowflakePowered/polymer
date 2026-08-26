import { createEffect, onCleanup, onMount } from "solid-js";
import { useInputActions } from "../../app/InputProvider";
import { useShell } from "../../app/ShellProvider";
import { PauseOverlay } from "./PauseOverlay";

export function PlayPage() {
  const shell = useShell();
  const input = useInputActions();
  createEffect(() => {
    if (shell.model.pauseOpen) shell.model.launch?.deactivateInput();
    else shell.model.launch?.activateInput();
  });
  onMount(() => {
    const togglePause = () => shell.setModel("pauseOpen", (open: boolean) => !open);
    const unsubscribe = input.subscribe((action) => {
      if (
        (action.type !== "pause" && !(action.type === "back" && action.sourceId === null))
        || action.event.defaultPrevented
      ) return;
      action.event.preventDefault();
      togglePause();
    });
    onCleanup(() => {
      unsubscribe();
      shell.model.launch?.activateInput();
    });
  });
  return <PauseOverlay />;
}
