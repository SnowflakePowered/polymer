import type { Notification } from "./notification-context";
import { SidebarButton } from "../SidebarButton";
import { ProgressIndicator } from "./ProgressIndicator";
export function SidebarJob(props: { notification: Notification; onOpen(): void }) {
  const item = () => props.notification;
  return <SidebarButton icon="info" label={`${item().title}: ${item().message}`} onClick={props.onOpen}>
    <ProgressIndicator label={item().title} completed={item().completed} total={item().total} />
  </SidebarButton>;
}
