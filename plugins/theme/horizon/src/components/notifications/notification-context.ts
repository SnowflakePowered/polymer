import { createContext, useContext, type Accessor } from "solid-js";
import type { Notification } from "../../../../../../js/notifications";
export type { Notification } from "../../../../../../js/notifications";
export const NotificationContext = createContext<{ items: Accessor<Notification[]>; dismiss(id: string): void; reopen(id: string): void }>();
export function useNotifications() { return useContext(NotificationContext)!; }
