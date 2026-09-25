import "./Toast.css";
import { useShell } from "../../app/shell-context";
import { For, Show } from "solid-js";
import { useNotifications } from "./notification-context";
import { ProgressToast } from "./ProgressToast";
export function ToastRegion() {
  const notifications = useNotifications();
  const shell = useShell();
  const hidden = () => !!shell.model.launch || shell.model.launcher.kind === "detecting" || shell.model.launcher.kind === "launching";
  return <Show when={!hidden()}><aside class="horizon-toast-region" aria-label="Notifications">
    <For each={notifications.items().filter(item => !item.dismissed)}>{item =>
      <ProgressToast notification={item} onDismiss={() => notifications.dismiss(item.id)} />
    }</For>
  </aside></Show>;
}
