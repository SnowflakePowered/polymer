import type { Notification } from "./notification-context";
import { JobIndicator } from "../../ui";
export function SidebarJob(props: { notification: Notification; onOpen(): void }) {
  return <JobIndicator title={props.notification.title} message={props.notification.message} completed={props.notification.completed} total={props.notification.total} onOpen={props.onOpen} />;
}
