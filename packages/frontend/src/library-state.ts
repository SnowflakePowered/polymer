import type { Job, JobsApi, LibraryApi, LibraryGame, LibraryGameQuery, LibraryImportResult } from "./polymer";

export type LibraryEntry = {
  path: string;
  name: string;
  platform: string;
  emulator: string;
  playedAt: number;
  artwork?: string;
  artworkBlurhash?: string;
  artworkRatio?: number;
  icon?: string;
  scraping?: { completed: number; total: number };
  missingKey?: { id: string };
};

/** Wide artwork for presenting one game, when its scrapers or package provide it. */
export type LibraryShowcase = { background?: string; logo?: string; cover?: string };
export type LibraryActivity = { id: string; title: string; message: string; state: "running" | "complete" | "error"; completed?: number; total?: number };
/** Activities contain running jobs and newly completed notices, not notification history. */
export type LibrarySnapshot = { games: LibraryEntry[]; total: number; filtered: boolean; status: string; activities: LibraryActivity[] };
export type LibraryQuery = Omit<LibraryGameQuery, "offset" | "limit">;

const PAGE_SIZE = 120;
const ARTWORK_KEYS = ["media.boxart.front", "media.icon", "banner.icon", "banner.banner"];

/** Catalog and scrape subscriptions, independent of theme rendering. */
export class LibraryState {
  private games: LibraryGame[] = [];
  private entries = new Map<string, LibraryEntry>();
  private query: LibraryQuery = {};
  private filtered = false;
  private limit = PAGE_SIZE;
  private total = 0;
  private artwork = new Map<string, Promise<string>>();
  private showcases = new Map<string, Promise<LibraryShowcase>>();
  private licenses = new Map<string, Promise<boolean>>();
  private loadingMore = false;
  private jobs = new Map<string, Job>();
  private sessionJobs = new Set<string>();
  private jobsLoaded = false;
  private imports = new Map<string, Job>();
  private firmwareActivities = new Map<string, LibraryActivity>();
  private firmwareRevision = 0;
  private revision = 0;
  private disposed = false;

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
    void this.loadJobs();
  }

  dispose() {
    this.disposed = true;
    this.library.removeEventListener("contentchange", this.refresh);
    this.jobsApi.removeEventListener("jobchange", this.jobChanged);
  }

  private async loadJobs() {
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
    }
  }

  setQuery(query: LibraryQuery) {
    this.query = query;
    this.limit = PAGE_SIZE;
    void this.load(0, this.limit);
  }

  loadMore() {
    if (this.loadingMore || this.games.length >= this.total) return;
    this.loadingMore = true;
    this.limit = this.games.length + PAGE_SIZE;
    void this.load(this.games.length, PAGE_SIZE).finally(() => { this.loadingMore = false; });
  }

  async import(path: string | FileSystemHandle) {
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
    this.showcases.clear();
    void this.load(0, this.limit, true);
  };

  /** Fetches a game's background, logo and box art by launchable path, preferring scraped media over package banners. */
  showcase(path: string): Promise<LibraryShowcase> {
    let showcase = this.showcases.get(path);
    if (!showcase) {
      showcase = (async () => {
        const launchable = await this.library.launchables.find(path);
        const game = launchable && await this.library.games.get(launchable.parentBinaryId ?? launchable.binaryId);
        if (!game) return {};
        const [background, logo, cover] = await Promise.all([
          this.media(game, ["media.background", "banner.background"], false),
          this.media(game, ["media.logo", "banner.logo"], false),
          this.media(game, ["media.boxart.front"], false),
        ]);
        return { background: background?.url, logo: logo?.url, cover: cover?.url };
      })();
      this.showcases.set(path, showcase);
      showcase.catch(() => this.showcases.delete(path));
    }
    return showcase;
  }

  private load(offset: number, limit: number, fresh = false): Promise<void> {
    const revision = offset ? this.revision : ++this.revision;
    const query = this.query;
    return this.library.games.list({ ...query, offset, limit, metadataKeys: ARTWORK_KEYS }).then(async ({ games, total }) => {
      const entries = await Promise.all(games.map(async game => {
        const launchable = game.launchables.find(item => !item.parentBinaryId) ?? game.launchables[0];
        if (!launchable) return [];
        const titleId = launchable.titleId;
        return [[game.id, { path: launchable.uri, name: game.title, platform: game.platform,
          missingKey: titleId && !await this.licensed(launchable.platform, titleId, fresh) ? { id: titleId } : undefined,
          emulator: "", playedAt: 0, ...await this.artworkFor(game, fresh),
          icon: (await this.media(game, ["media.icon", "banner.icon", "banner.banner"], fresh))?.url }] as const];
      }));
      if (this.disposed || revision !== this.revision) return;
      if (!offset && limit !== this.limit) { void this.load(0, this.limit, fresh); return; }
      this.games = offset ? [...this.games, ...games] : games;
      this.filtered = !!query.text?.trim() || !!query.platforms?.length;
      this.total = total;
      this.entries = new Map<string, LibraryEntry>([...(offset ? this.entries : []), ...entries.flat()]);
      this.publish();
    }).catch(error => {
      if (!this.disposed && revision === this.revision) this.failed(error);
    });
  }

  private jobChanged = (event: Event) => {
    const job = (event as CustomEvent<Job>).detail;
    if (job.kind === "import") return this.importChanged(job);
    if (job.kind === "check") {
      if (job.state === "completed") this.refresh();
      return;
    }
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

  private licensed(platform: string, titleId: string, fresh: boolean): Promise<boolean> {
    const key = `${platform}:${titleId}`;
    let licensed = fresh ? undefined : this.licenses.get(key);
    if (!licensed) {
      licensed = this.library.keys.licensed(platform, titleId);
      this.licenses.set(key, licensed);
      licensed.catch(() => this.licenses.delete(key));
    }
    return licensed;
  }

  private async artworkFor(game: LibraryGame, fresh: boolean): Promise<Pick<LibraryEntry, "artwork" | "artworkBlurhash" | "artworkRatio">> {
    const media = await this.media(game, ["media.boxart.front"], fresh);
    const data = media?.metadata.data.kind === "media" ? media.metadata.data : undefined;
    return {
      artwork: media?.url,
      artworkBlurhash: data?.blurhash ?? undefined,
      artworkRatio: data?.width && data.height ? data.width / data.height : undefined,
    };
  }

  private async media(game: LibraryGame, keys: readonly string[], fresh: boolean): Promise<{ url: string; metadata: LibraryGame["metadata"][number] } | undefined> {
    const candidates = game.metadata.filter(item =>
      keys.includes(item.key) && item.data.kind === "media" && item.data.mediaType.startsWith("image/")
    ).sort((a, b) => keys.indexOf(a.key) - keys.indexOf(b.key) || a.ordinal - b.ordinal);
    for (const candidate of candidates) {
      try {
        let url = fresh ? undefined : this.artwork.get(candidate.id);
        if (!url) {
          url = this.library.metadata.url(candidate.id);
          this.artwork.set(candidate.id, url);
          url.catch(() => this.artwork.delete(candidate.id));
        }
        return { url: await url, metadata: candidate };
      } catch (error) {
        console.warn(`Unable to load artwork for ${game.id}: ${candidate.id}`, error);
      }
    }
    return undefined;
  }

  private progress(job: Job): string {
    const game = this.games.find(game => game.id === job.subject.binaryId);
    const name = game?.title ?? "Game";
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
      total: this.total,
      filtered: this.filtered,
      activities,
      status: importing.map(job => `${job.message ?? `Importing ${job.title}`}…`).concat(visible.map(job => this.progress(job))).join(" · "),
    });
  }
}
