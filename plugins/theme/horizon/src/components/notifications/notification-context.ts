import { createContext, useContext, type Accessor } from "solid-js";
export type Notification = { id: string; title: string; message: string; state: "running" | "complete" | "error" | "info"; icon?: string; completed?: number; total?: number; dismissed: boolean };
export const NotificationContext = createContext<{ items: Accessor<Notification[]>; dismiss(id: string): void; reopen(id: string): void }>();
export function useNotifications() { return useContext(NotificationContext)!; }
