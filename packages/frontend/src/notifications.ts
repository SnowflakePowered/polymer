import type { Job, JobsApi, MachineSpec } from "./polymer";

export type NotificationState = "running" | "complete" | "error" | "info";
export type Notification = { id: string; title: string; message: string; state: NotificationState; icon?: string; completed?: number; total?: number; dismissed: boolean };

/**
 * Background job notices with toast expiry. Job state is kept after its toast expires, and
 * progress updates do not restart expiry. Scrape and import jobs are left to the library state.
 */
export class NotificationCenter {
  private items = new Map<string, Notification>();
  private timers = new Map<string, ReturnType<typeof setTimeout>>();

  constructor(
    private polymer: Readonly<{ jobs: JobsApi; machine(id: string): MachineSpec | null }>,
    private changed: (items: Notification[]) => void,
  ) {}

  start() {
    this.polymer.jobs.addEventListener("jobchange", this.jobChanged);
    // Late listeners restore running work, not historical completion notices.
    void this.polymer.jobs.list().then(jobs => jobs.filter(job => job.state === "queued" || job.state === "running").forEach(this.receive));
  }

  dispose() {
    this.polymer.jobs.removeEventListener("jobchange", this.jobChanged);
    for (const timer of this.timers.values()) clearTimeout(timer);
    this.timers.clear();
  }

  upsert(next: Omit<Notification, "dismissed">) {
    const previous = this.items.get(next.id);
    const sameNotice = previous?.state === next.state
      && (next.state === "running" || previous.message === next.message);
    this.items.set(next.id, { ...next, dismissed: sameNotice ? previous!.dismissed : false });
    if (!sameNotice) this.schedule(next.id);
    this.publish();
  }

  remove(id: string) {
    clearTimeout(this.timers.get(id));
    this.timers.delete(id);
    if (this.items.delete(id)) this.publish();
  }

  dismiss(id: string) {
    const item = this.items.get(id);
    if (!item) return;
    if (item.state !== "running") return this.remove(id);
    clearTimeout(this.timers.get(id));
    this.timers.delete(id);
    this.items.set(id, { ...item, dismissed: true });
    this.publish();
  }

  reopen(id: string) {
    const item = this.items.get(id);
    if (!item) return;
    this.items.set(id, { ...item, dismissed: false });
    this.schedule(id);
    this.publish();
  }

  private publish() { this.changed([...this.items.values()]); }

  private schedule(id: string) {
    clearTimeout(this.timers.get(id));
    const item = this.items.get(id);
    if (!item || item.dismissed) return;
    const duration = item.state === "error" ? 8000 : item.state === "running" ? 5000 : 3500;
    this.timers.set(id, setTimeout(() => this.dismiss(id), duration));
  }

  private readonly jobChanged = (event: Event) => this.receive((event as CustomEvent<Job>).detail);

  private readonly receive = (job: Job) => {
    if (job.kind === "scrape" || job.kind === "import") return;
    const id = `job:${job.id}`;
    if (job.kind === "plugin" && job.state === "completed") return this.remove(id);
    const active = job.state === "queued" || job.state === "running" || job.state === "paused";
    const plugin = job.subject.pluginId;
    this.upsert({
      id, title: plugin ? this.polymer.machine(plugin)?.name ?? job.title : job.title, icon: job.icon ?? undefined,
      message: job.error ?? job.message ?? "Waiting",
      state: active ? "running" : job.state === "completed" ? "complete" : "error",
      completed: active ? job.completed ?? undefined : undefined, total: active ? job.total ?? undefined : undefined,
    });
  };
}
