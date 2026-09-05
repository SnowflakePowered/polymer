import type { LibraryApi, LibraryGame, ScrapingApi, ScrapeJob } from "./polymer";

export type LibraryEntry = {
  path: string;
  name: string;
  platform: string;
  emulator: string;
  playedAt: number;
};

export type LibrarySnapshot = { games: LibraryEntry[]; status: string };

/** Catalog and scrape subscriptions, independent of theme rendering. */
export class LibraryState {
  private games: LibraryGame[] = [];
  private jobs = new Map<string, ScrapeJob>();
  private imports = new Map<symbol, string>();
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
        this.jobs.set(job.id, job);
        changed = true;
      }
      if (changed) this.refresh();
      this.publish();
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
      const { launchable, scrapeJob } = await this.library.launchables.add({ path });
      const job = scrapeJob ?? await this.scraping.jobs.start(
        launchable.parentBinaryId ?? launchable.binaryId,
        { triggerBinaryId: launchable.binaryId },
      );
      // Notifications may already have advanced beyond the enqueue response.
      if (!this.jobs.has(job.id)) this.jobs.set(job.id, job);
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
    void this.library.games.list().then(games => {
      if (this.disposed || revision !== this.revision) return;
      this.games = games;
      this.publish();
    }).catch(error => {
      if (!this.disposed && revision === this.revision) this.failed(error);
    });
  };

  private jobChanged = (event: Event) => {
    const job = (event as CustomEvent<ScrapeJob>).detail;
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
      games: this.games.flatMap(game => {
        const launchable = game.launchables.find(item => !item.parentBinaryId) ?? game.launchables[0];
        return launchable ? [{ path: launchable.path, name: this.title(game), platform: game.platform, emulator: "", playedAt: 0 }] : [];
      }),
      status: [...this.imports.values()].map(name => `Identifying ${name}…`).concat(visible.map(job => this.progress(job))).join(" · "),
    });
  }
}
