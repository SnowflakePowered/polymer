import { Show } from "solid-js";
import type { Notification } from "./notification-context";
import { Toast } from "../../ui";
import { Progress as ProgressIndicator } from "../../ui";
export function ProgressToast(props: { notification: Notification; onDismiss(): void }) {
  const item = () => props.notification;
  const percent = () => item().total !== undefined && item().total! > 0 && item().completed !== undefined ? Math.max(0, Math.min(100, Math.round(item().completed! / item().total! * 100))) : undefined;
  return <Toast title={item().title} message={item().message} icon={item().icon} error={item().state === "error"} onDismiss={props.onDismiss}>
    <Show when={item().state === "running"}>
      <Show when={percent() !== undefined}><span class="horizon-toast-percent">{percent()}%</span></Show>
      <ProgressIndicator label={item().title} completed={item().completed} total={item().total} />
    </Show>
  </Toast>;
}
