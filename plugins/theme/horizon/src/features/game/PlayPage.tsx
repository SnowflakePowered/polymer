import { useInputScope } from "../../ui";
import { useShell } from "../../app/shell-context";
import { PauseOverlay } from "./PauseOverlay";
export function PlayPage() {
  const shell = useShell();
  useInputScope(() => ({ root: document.body, handle: action => {
    if (action.type !== "pause" && !(action.type === "back" && action.origin === "keyboard")) return false;
    shell.setModel("pauseOpen", (open: boolean) => !open);
    return true;
  } }));
  return <PauseOverlay />;
}
