import { For } from "solid-js";
import { useNotifications } from "./notification-context";
import { SidebarJob } from "./SidebarJob";
import "./SidebarJobs.css";
export function SidebarJobs() {
  const notifications = useNotifications();
  return <div class="horizon-sidebar-jobs" aria-label="Background jobs">
    <For each={notifications.items().filter(item => item.dismissed && item.state === "running")}>{item =>
      <SidebarJob notification={item} onOpen={() => notifications.reopen(item.id)} />
    }</For>
  </div>;
}
