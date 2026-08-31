import { onCleanup, onMount } from "solid-js";
import { useInputActions } from "../../app/InputProvider";
import { useShell } from "../../app/ShellProvider";
import { PauseOverlay } from "./PauseOverlay";

export function PlayPage() {
  const shell = useShell();
  const input = useInputActions();
  onMount(() => {
    const togglePause = () => shell.setModel("pauseOpen", (open: boolean) => !open);
    const unsubscribe = input.subscribe((action) => {
      if (
        (action.type !== "pause" && !(action.type === "back" && action.origin === "keyboard"))
        || action.event.defaultPrevented
      ) return;
      action.event.preventDefault();
      togglePause();
    });
    onCleanup(unsubscribe);
  });
  return <PauseOverlay />;
}
