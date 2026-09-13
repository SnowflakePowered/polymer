import { createStore, reconcile } from "solid-js/store";
import type { Notification } from "./notification-context";

/** Keeps job state after its toast expires, without restarting expiry on progress updates. */
export class NotificationStore {
  private readonly state = createStore<Notification[]>([]);
  private readonly timers = new Map<string, ReturnType<typeof setTimeout>>();
  readonly items = () => this.state[0];

  upsert(next: Omit<Notification, "dismissed">) {
    const previous = this.items().find(item => item.id === next.id);
    const sameNotice = previous?.state === next.state
      && (next.state === "running" || previous.message === next.message);
    const item = { ...next, dismissed: sameNotice ? previous!.dismissed : false };
    this.state[1](reconcile(previous
      ? this.items().map(value => value.id === next.id ? item : value)
      : [...this.items(), item], { key: "id" }));
    if (!sameNotice) this.schedule(next.id);
  }

  remove(id: string) {
    clearTimeout(this.timers.get(id));
    this.timers.delete(id);
    this.state[1](reconcile(this.items().filter(item => item.id !== id), { key: "id" }));
  }

  dismiss(id: string) {
    clearTimeout(this.timers.get(id));
    this.timers.delete(id);
    this.state[1](item => item.id === id, "dismissed", true);
  }

  reopen(id: string) {
    this.state[1](item => item.id === id, "dismissed", false);
    this.schedule(id);
  }

  dispose() {
    for (const timer of this.timers.values()) clearTimeout(timer);
    this.timers.clear();
  }

  private schedule(id: string) {
    clearTimeout(this.timers.get(id));
    const item = this.items().find(item => item.id === id);
    if (!item || item.dismissed) return;
    const duration = item.state === "error" ? 8000 : item.state === "running" ? 5000 : 3500;
    this.timers.set(id, setTimeout(() => this.dismiss(id), duration));
  }
}
