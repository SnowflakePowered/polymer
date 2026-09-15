import type { LibraryApi, LibraryGame, ScrapingApi, ScrapeJob } from "./polymer";

export type LibraryEntry = {
  path: string;
  name: string;
  platform: string;
  emulator: string;
  playedAt: number;
  artwork?: string;
};

export type LibraryActivity = { id: string; title: string; message: string; state: "running" | "complete" | "error"; completed?: number; total?: number };
export type LibrarySnapshot = { games: LibraryEntry[]; status: string; activities: LibraryActivity[] };

/** Catalog and scrape subscriptions, independent of theme rendering. */
export class LibraryState {
  private games: LibraryGame[] = [];
  private entries: LibraryEntry[] = [];
  private jobs = new Map<string, ScrapeJob>();
  private sessionJobs = new Set<string>();
  private jobsLoaded = false;
  private imports = new Map<symbol, string>();
  private firmwareActivities = new Map<string, LibraryActivity>();
  private firmwareRevision = 0;
  private revision = 0;
  private disposed = false;
  private pollTimer: ReturnType<typeof setTimeout> | undefined;

  constructor(
    private library: LibraryApi,
    private scraping: ScrapingApi,
    private changed: (snapshot: LibrarySnapshot) => void,
    private failed: (error: unknown) => void,
  ) {}

  start() {
    this.library.addEventListener("contentchange", this.refresh);
    this.scraping.addEventListener("jobstatechange", this.jobChanged);
    this.refresh();
    void this.pollJobs();
  }

  dispose() {
    this.disposed = true;
    clearTimeout(this.pollTimer);
    this.library.removeEventListener("contentchange", this.refresh);
    this.scraping.removeEventListener("jobstatechange", this.jobChanged);
  }

  private async pollJobs() {
    const previous = new Map(this.jobs);
    try {
      const jobs = await this.scraping.jobs.list();
      if (this.disposed) return;
      let changed = false;
      for (const job of jobs) {
        // A notification received during the request is newer than this snapshot.
        if (this.jobs.get(job.id) !== previous.get(job.id)) continue;
        if (JSON.stringify(this.jobs.get(job.id)) === JSON.stringify(job)) continue;
        if (this.jobsLoaded || job.state === "queued" || job.state === "running") this.sessionJobs.add(job.id);
        this.jobs.set(job.id, job);
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
    const token = Symbol();
    this.imports.set(token, path.split(/[\\/]/).pop() || path);
    this.publish();
    try {
      const result = await this.library.import({ path });
      if (result.kind === "firmware") {
        const revision = ++this.firmwareRevision;
        for (const item of result.firmware) {
          const key = JSON.stringify([item.platform, item.type, item.label]);
          this.firmwareActivities.set(key, {
            id: `firmware:${key}:${revision}`, title: "Firmware installed",
            message: `${item.label} (${item.platform})`, state: "complete",
          });
        }
        return;
      }
      for (const { launchable, scrapeJob } of result.imports) {
        const job = scrapeJob ?? await this.scraping.jobs.start(
          launchable.parentBinaryId ?? launchable.binaryId,
          { triggerBinaryId: launchable.binaryId },
        );
        // Notifications may already have advanced beyond the enqueue response.
        this.sessionJobs.add(job.id);
        if (!this.jobs.has(job.id)) this.jobs.set(job.id, job);
      }
      this.refresh();
    } catch (error) {
      if (!this.disposed) this.failed(error);
    } finally {
      this.imports.delete(token);
      this.publish();
    }
  }

  private refresh = () => {
    const revision = ++this.revision;
    void this.library.games.list().then(async games => {
      const entries = await Promise.all(games.map(async game => {
        const launchable = game.launchables.find(item => !item.parentBinaryId) ?? game.launchables[0];
        if (!launchable) return [];
        return [{ path: launchable.path, name: this.title(game), platform: game.platform,
          emulator: "", playedAt: 0, artwork: await this.artwork(game) }];
      }));
      if (this.disposed || revision !== this.revision) return;
      this.games = games;
      this.entries = entries.flat();
      this.publish();
    }).catch(error => {
      if (!this.disposed && revision === this.revision) this.failed(error);
    });
  };

  private jobChanged = (event: Event) => {
    const job = (event as CustomEvent<ScrapeJob>).detail;
    this.sessionJobs.add(job.id);
    this.jobs.set(job.id, job);
    this.publish();
    if (job.state !== "queued" && job.state !== "running") this.refresh();
  };

  private title(game: LibraryGame): string {
    const titles = game.metadata.filter(item => item.key === "meta.title" || item.key.startsWith("meta.title."));
    const title = titles.find(item => item.key === "meta.title") ?? titles[0];
    if (title?.data.kind === "scalar" && typeof title.data.value === "string") return title.data.value;
    return game.launchables[0]?.path.split(/[\\/]/).pop() || game.id;
  }

  private async artwork(game: LibraryGame): Promise<string | undefined> {
    const keys = ["media.boxart.front", "media.banner", "media.logo", "media.boxart.full"];
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

  private progress(job: ScrapeJob): string {
    const game = this.games.find(game => game.id === job.rootBinaryId);
    const name = game ? this.title(game) : "Game";
    const done = job.attempts.filter(attempt => attempt.state === "completed" || attempt.state === "failed").length;
    const active = job.attempts.filter(attempt => attempt.state === "running").map(attempt => attempt.pluginId);
    const errors = job.attempts.filter(attempt => attempt.error).map(attempt => `${attempt.pluginId}: ${attempt.error}`);
    const detail = errors.length ? errors.join("; ") : active.join(", ");
    const state = job.state === "completed" && !job.attempts.length ? "No matching scrapers" : `Scraping ${job.state}`;
    return `${name}: ${state} (${done}/${job.attempts.length} scrapers)${detail ? ` — ${detail}` : ""}`;
  }

  private publish() {
    if (this.disposed) return;
    const jobs = [...this.jobs.values()].sort((a, b) => b.updatedAt - a.updatedAt);
    const active = jobs.filter(job => job.state === "queued" || job.state === "running");
    const visible = active.length ? active : jobs.slice(0, 1);
    this.changed({
      games: this.entries,
      activities: [
        ...this.firmwareActivities.values(),
        ...(this.imports.size ? [{ id: "imports", title: "Identifying games and firmware", message: [...this.imports.values()].join(", "), state: "running" as const }] : []),
        ...jobs.filter(job => this.sessionJobs.has(job.id)).map(job => ({ id: job.id, title: "Scraping game library", message: this.progress(job),
          state: (job.state === "queued" || job.state === "running" ? "running" : job.state === "completed" ? "complete" : "error") as LibraryActivity["state"],
          completed: job.attempts.filter(attempt => attempt.state === "completed" || attempt.state === "failed").length,
          total: job.attempts.length,
        })),
      ],
      status: [...this.imports.values()].map(name => `Identifying ${name}…`).concat(visible.map(job => this.progress(job))).join(" · "),
    });
  }
}
