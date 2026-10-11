import { createContext, useContext, type Accessor } from "solid-js";
import type { Notification } from "@polymer-fe/frontend/notifications";
export type { Notification } from "@polymer-fe/frontend/notifications";
export const NotificationContext = createContext<{ items: Accessor<Notification[]>; dismiss(id: string): void; reopen(id: string): void }>();
export function useNotifications() { return useContext(NotificationContext)!; }
