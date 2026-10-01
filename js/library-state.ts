import type { Job, JobsApi, LibraryApi, LibraryGame, LibraryImportResult } from "./polymer";

export type LibraryEntry = {
  path: string;
  name: string;
  platform: string;
  emulator: string;
  playedAt: number;
  artwork?: string;
  icon?: string;
  scraping?: { completed: number; total: number };
  missingKey?: { id: string; filename: string };
};

export type LibraryActivity = { id: string; title: string; message: string; state: "running" | "complete" | "error"; completed?: number; total?: number };
/** Activities contain running jobs and newly completed notices, not notification history. */
export type LibrarySnapshot = { games: LibraryEntry[]; status: string; activities: LibraryActivity[] };

/** Catalog and scrape subscriptions, independent of theme rendering. */
export class LibraryState {
  private games: LibraryGame[] = [];
  private entries = new Map<string, LibraryEntry>();
  private jobs = new Map<string, Job>();
  private sessionJobs = new Set<string>();
  private jobsLoaded = false;
  private imports = new Map<string, Job>();
  private firmwareActivities = new Map<string, LibraryActivity>();
  private firmwareRevision = 0;
  private revision = 0;
  private disposed = false;
  private pollTimer: ReturnType<typeof setTimeout> | undefined;

  constructor(
    private library: LibraryApi,
    private jobsApi: JobsApi,
    private changed: (snapshot: LibrarySnapshot) => void,
    private failed: (error: unknown) => void,
  ) {}

  start() {
    this.library.addEventListener("contentchange", this.refresh);
    this.jobsApi.addEventListener("jobchange", this.jobChanged);
    this.refresh();
    void this.pollJobs();
  }

  dispose() {
    this.disposed = true;
    clearTimeout(this.pollTimer);
    this.library.removeEventListener("contentchange", this.refresh);
    this.jobsApi.removeEventListener("jobchange", this.jobChanged);
  }

  private async pollJobs() {
    const previous = new Map(this.jobs);
    try {
      const listed = await this.jobsApi.list();
      if (this.disposed) return;
      // Imports resumed after a restart may have started before this listener.
      listed.filter(job => job.kind === "import" && (job.state === "queued" || job.state === "running")).forEach(this.importChanged);
      const jobs = listed.filter(job => job.kind === "scrape");
      let changed = false;
      for (const job of jobs) {
        // A notification received during the request is newer than this snapshot.
        if (this.jobs.get(job.id) !== previous.get(job.id)) continue;
        if (JSON.stringify(this.jobs.get(job.id)) === JSON.stringify(job)) continue;
        this.updateJob(job, this.jobsLoaded || job.state === "queued" || job.state === "running");
        changed = true;
      }
      this.jobsLoaded = true;
      if (changed) {
        this.refresh();
        this.publish();
      }
    } catch (error) {
      if (!this.disposed) this.failed(error);
    } finally {
      if (!this.disposed) this.pollTimer = setTimeout(() => void this.pollJobs(), 1000);
    }
  }

  async import(path: string) {
    try {
      this.importChanged(await this.library.import({ path }));
    } catch (error) {
      if (!this.disposed) this.failed(error);
    }
  }

  private importChanged = (job: Job) => {
    const previous = this.imports.get(job.id);
    // Notifications may already have advanced beyond the enqueue response.
    if (previous && previous.updatedAt >= job.updatedAt) return;
    const active = (state: Job["state"]) => state === "queued" || state === "running";
    this.imports.set(job.id, job);
    if (!active(job.state) && (!previous || active(previous.state))) {
      if (job.state === "failed") this.failed(new Error(job.error ?? `Could not import ${job.title}`));
      else this.announce((job.detail as { result?: LibraryImportResult } | null)?.result);
      this.refresh();
    }
    this.publish();
  };

  private announce(result: LibraryImportResult | undefined) {
    const revision = ++this.firmwareRevision;
    if (result?.kind === "firmware") {
      const item = result.firmware;
      const key = JSON.stringify([item.platform, item.label]);
      this.firmwareActivities.set(key, {
        id: `firmware:${key}:${revision}`, title: "Firmware installed",
        message: `${item.label} (${item.platform})`, state: "complete",
      });
    } else if (result?.kind === "keys") {
      this.firmwareActivities.set(`keys:${result.platform}`, {
        id: `keys:${result.platform}:${revision}`, title: "Keys imported",
        message: `${result.count} keys (${result.platform})`, state: "complete",
      });
    } else if (result?.kind === "system") {
      this.firmwareActivities.set(`system:${result.platform}`, {
        id: `system:${result.platform}:${revision}`, title: "System software installed",
        message: `${result.version} (${result.platform})`, state: "complete",
      });
    }
  }

  private refresh = () => {
    const revision = ++this.revision;
    void this.library.games.list().then(async games => {
      const licenses = games.some(game => game.platform === "SONY_PS3")
        ? new Set((await this.library.keys.list("SONY_PS3", { namespace: "content" })).map(key => key.id))
        : new Set<string>();
      const entries = await Promise.all(games.map(async game => {
        const launchable = game.launchables.find(item => !item.parentBinaryId) ?? game.launchables[0];
        if (!launchable) return [];
        const license = launchable.platform === "SONY_PS3" && launchable.titleId
          && /^[A-Za-z0-9]{6}-[A-Za-z]{4}\d{5}_\d{2}-[A-Za-z0-9_-]{16}$/.test(launchable.titleId) ? launchable.titleId : null;
        return [[game.id, { path: launchable.uri, name: this.title(game), platform: game.platform,
          missingKey: license && !licenses.has(license) ? { id: license, filename: `${license}.rap` } : undefined,
          emulator: "", playedAt: 0, artwork: await this.media(game, ["media.boxart.front"]),
          icon: await this.media(game, ["media.icon", "banner.icon", "banner.banner"]) }] as const];
      }));
      if (this.disposed || revision !== this.revision) return;
      this.games = games;
      this.entries = new Map(entries.flat());
      this.publish();
    }).catch(error => {
      if (!this.disposed && revision === this.revision) this.failed(error);
    });
  };

  private jobChanged = (event: Event) => {
    const job = (event as CustomEvent<Job>).detail;
    if (job.kind === "import") return this.importChanged(job);
    if (job.kind !== "scrape") return;
    if (JSON.stringify(this.jobs.get(job.id)) === JSON.stringify(job)) return;
    this.updateJob(job, true);
    this.publish();
    if (job.state !== "queued" && job.state !== "running") this.refresh();
  };

  private updateJob(job: Job, notify: boolean) {
    const previous = this.jobs.get(job.id);
    // Terminal jobs are announced once. Later snapshots and metadata refreshes
    // must not recreate an expired or dismissed completion notice.
    if (notify && (job.state === "queued" || job.state === "running" || previous?.state !== job.state)) {
      this.sessionJobs.add(job.id);
    }
    this.jobs.set(job.id, job);
  }

  private title(game: LibraryGame): string {
    const titles = game.metadata.filter(item => item.key === "meta.title" || item.key.startsWith("meta.title."));
    const title = titles.find(item => item.key === "meta.title") ?? titles[0];
    if (title?.data.kind === "scalar" && typeof title.data.value === "string") return title.data.value;
    const internal = game.metadata.find(item => item.key === "package.internalname");
    if (internal?.data.kind === "scalar" && typeof internal.data.value === "string") return internal.data.value;
    return game.launchables[0]?.uri.split("/").pop() || game.id;
  }

  private async media(game: LibraryGame, keys: readonly string[]): Promise<string | undefined> {
    const candidates = game.metadata.filter(item =>
      keys.includes(item.key) && item.data.kind === "media" && item.data.mediaType.startsWith("image/")
    ).sort((a, b) => keys.indexOf(a.key) - keys.indexOf(b.key) || a.ordinal - b.ordinal);
    for (const candidate of candidates) {
      try {
        return await this.library.metadata.url(candidate.id);
      } catch (error) {
        console.warn(`Unable to load artwork for ${game.id}: ${candidate.id}`, error);
      }
    }
    return undefined;
  }

  private progress(job: Job): string {
    const game = this.games.find(game => game.id === job.subject.binaryId);
    const name = game ? this.title(game) : "Game";
    const done = job.steps.filter(step => step.state === "completed" || step.state === "failed").length;
    const active = job.steps.filter(step => step.state === "running").map(step => step.key);
    const errors = [...job.steps.filter(step => step.error).map(step => `${step.key}: ${step.error}`), ...(job.error ? [job.error] : [])];
    const detail = errors.length ? errors.join("; ") : active.join(", ");
    const state = job.state === "completed" && !job.steps.length ? "No matching scrapers" : `Scraping ${job.state}`;
    return `${name}: ${state} (${done}/${job.steps.length} scrapers)${detail ? ` — ${detail}` : ""}`;
  }

  private publish() {
    if (this.disposed) return;
    const importing = [...this.imports.values()].filter(job => job.state === "queued" || job.state === "running");
    const jobs = [...this.jobs.values()].sort((a, b) => b.updatedAt - a.updatedAt);
    const active = jobs.filter(job => job.state === "queued" || job.state === "running");
    const visible = active.length ? active : jobs.slice(0, 1);
    const activities: LibraryActivity[] = [
      ...this.firmwareActivities.values(),
      ...(importing.length ? [{ id: "imports", title: "Importing games and firmware", message: importing.map(job => job.title).join(", "), state: "running" as const }] : []),
      ...jobs.filter(job => this.sessionJobs.has(job.id) && job.state !== "queued" && job.state !== "running").map(job => ({ id: job.id, title: "Scraping game library", message: this.progress(job),
        state: (job.state === "completed" ? "complete" : "error") as LibraryActivity["state"],
        completed: job.steps.filter(step => step.state === "completed" || step.state === "failed").length,
        total: job.steps.length,
      })),
    ];
    this.firmwareActivities.clear();
    for (const activity of activities) {
      if (activity.state !== "running") this.sessionJobs.delete(activity.id);
    }
    this.changed({
      games: [...this.entries].map(([id, entry]) => {
        const gameJobs = active.filter(job => job.subject.binaryId === id);
        const steps = gameJobs.flatMap(job => job.steps);
        return { ...entry, scraping: gameJobs.length ? {
          completed: steps.filter(step => step.state === "completed" || step.state === "failed").length,
          total: steps.length,
        } : undefined };
      }),
      activities,
      status: importing.map(job => `${job.message ?? `Importing ${job.title}`}…`).concat(visible.map(job => this.progress(job))).join(" · "),
    });
  }
}
