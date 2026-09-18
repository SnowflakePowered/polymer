import { createStore, produce, reconcile } from "solid-js/store";
import type { Notification } from "./notification-context";

/** Keeps job state after its toast expires, without restarting expiry on progress updates. */
export class NotificationStore {
  private readonly state = createStore<Record<string, Notification>>({});
  private readonly timers = new Map<string, ReturnType<typeof setTimeout>>();
  readonly items = () => Object.values(this.state[0]);

  upsert(next: Omit<Notification, "dismissed">) {
    const previous = this.state[0][next.id];
    const sameNotice = previous?.state === next.state
      && (next.state === "running" || previous.message === next.message);
    const item = { ...next, dismissed: sameNotice ? previous!.dismissed : false };
    this.state[1](next.id, reconcile(item));
    if (!sameNotice) this.schedule(next.id);
  }

  remove(id: string) {
    clearTimeout(this.timers.get(id));
    this.timers.delete(id);
    this.state[1](produce(items => { delete items[id]; }));
  }

  dismiss(id: string) {
    const item = this.state[0][id];
    if (!item) return;
    if (item.state !== "running") {
      this.remove(id);
      return;
    }
    clearTimeout(this.timers.get(id));
    this.timers.delete(id);
    this.state[1](id, "dismissed", true);
  }

  reopen(id: string) {
    if (!this.state[0][id]) return;
    this.state[1](id, "dismissed", false);
    this.schedule(id);
  }

  dispose() {
    for (const timer of this.timers.values()) clearTimeout(timer);
    this.timers.clear();
  }

  private schedule(id: string) {
    clearTimeout(this.timers.get(id));
    const item = this.state[0][id];
    if (!item || item.dismissed) return;
    const duration = item.state === "error" ? 8000 : item.state === "running" ? 5000 : 3500;
    this.timers.set(id, setTimeout(() => this.dismiss(id), duration));
  }
}
