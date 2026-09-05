import { For, Show } from "solid-js";
import { useShell } from "../app/shell-context";
import { Icon } from "./Icon";
import "./ControllerConnectionToast.css";

export function ControllerConnectionToast() {
  const shell = useShell();
  return <Show when={shell.model.controllerToast}>{(toast) =>
    <aside class="horizon-controller-toast" role="status" aria-live="polite">
      <Icon name="gamepad" />
      <div><strong>{toast().name}</strong><span>Connected as player {toast().index + 1}</span></div>
      <div class="horizon-toast-index" aria-label={`Player ${toast().index + 1}`}>
        <For each={Array.from({ length: toast().index + 1 })}>{() => <i />}</For>
      </div>
    </aside>
  }</Show>;
}
