/**
 * Keeps an emulator's binaries up to date in a plugin cache. Releases come from the
 * emulator index; each update is downloaded, extracted, and staged, and only becomes
 * active when the next launch promotes it, so a running emulator is never replaced.
 *
 * @module
 */

/** An archive to install into a generation. */
export type RuntimeSource = Readonly<{
  version: string;
  url: string;
  sha256?: string;
  size?: number;
  /** Generation-relative directory the archive is extracted into. */
  directory?: string;
}>;

/** An entry listed by a cache directory. */
export interface CacheEntry {
  readonly name: string;
  readonly kind: "file" | "directory";
}

/** A directory inside a cache. */
export interface CacheDirectory {
  delete(): Promise<void>;
}

/** A cache the updater stores archives and generations in. */
export interface Cache<File, Directory extends CacheDirectory, Entry extends CacheEntry>
  extends AsyncIterable<Entry> {
  open(path: string): File;
  directory(path: string): Directory;
  remove(name: string): Promise<void>;
}

/** A prepared SQL statement. */
export interface Statement {
  get<T = Record<string, unknown>>(values?: readonly unknown[]): Promise<T | undefined>;
  all<T = Record<string, unknown>>(values?: readonly unknown[]): Promise<T[]>;
  run(values?: readonly unknown[]): Promise<{ changes: number; lastInsertRowid: number }>;
}

/** A connection to the plugin's database. */
export interface Database {
  prepare(sql: string): Statement;
  exec(sql: string): Promise<void>;
  transactionAsync<T>(callback: (database: Database) => Promise<T>): Promise<T>;
  close(): Promise<void>;
}

/** The host services the updater uses. A Polymer plugin passes its `polymer:*` modules. */
export interface EmulatorUpdatesHost<File, Directory extends CacheDirectory, Entry extends CacheEntry> {
  /** Shown in job titles and errors. */
  readonly name: string;
  readonly cache: Cache<File, Directory, Entry>;
  /** Asset key in the emulator index, such as `win-x64`. */
  readonly osIdentifier: string;
  /** Preferred release channel; stable, preview, and nightly are tried after it. */
  readonly channel?: string;
  /** Table that records generations, so updaters sharing a database stay independent. */
  readonly table?: string;
  connect(): Promise<Database>;
  download(url: string, file: File, options?: { sha256?: string; byteLength?: number }): Promise<void>;
  unarchive(file: File, directory: Directory, options?: { stripRoot?: boolean }): Promise<void>;
  readonly Job: new (title: string) => { run<T>(work: () => T | PromiseLike<T>): Promise<T> };
}

/** A directory entry of the cache, which is what a generation is. */
export type CacheDirectoryEntry<Entry extends CacheEntry> = Extract<Entry, { kind: "directory" }>;

type Generation = { id: number };

const INDEX_URL = "https://emulator-index.polymer.gg/";

export class EmulatorUpdates<File, Directory extends CacheDirectory, Entry extends CacheEntry> {
  readonly #host: EmulatorUpdatesHost<File, Directory, Entry>;
  readonly #table: string;

  constructor(host: EmulatorUpdatesHost<File, Directory, Entry>) {
    this.#host = host;
    this.#table = host.table ?? "runtime_generations";
    if (!/^[A-Za-z_]\w*$/.test(this.#table)) throw new TypeError(`Invalid generations table ${this.#table}`);
  }

  /** The newest release of an emulator in the index for this host. */
  async release(emulator: string): Promise<RuntimeSource> {
    const { osIdentifier, channel = "stable" } = this.#host;
    const response = await fetch(INDEX_URL);
    if (!response.ok) throw new Error(`Emulator index request failed with ${response.status}`);
    const channels = (await response.json()).emulators?.[emulator];
    const release = [channel, "stable", "preview", "nightly"]
      .flatMap(name => channels?.[name] ?? [])
      .find(release => release.assets?.[osIdentifier]);
    if (!release) throw new Error(`${emulator} has no ${osIdentifier} release`);
    return { version: release.version, ...release.assets[osIdentifier] };
  }

  /** Whether a generation is installed. */
  async ready(): Promise<boolean> {
    const installed = await this.#query(database => database
      .prepare(`SELECT id FROM ${this.#table} WHERE state IN ('pending', 'active')`)
      .all<Generation>());
    for (const { id } of installed) {
      if (await this.#generation(id)) return true;
    }
    return false;
  }

  /** Installs the sources as the pending generation, unless they already are installed. */
  update(sources: readonly RuntimeSource[]): Promise<void> {
    return new this.#host.Job(`Updating ${this.#host.name}`).run(async () => {
      const version = sources.map(source => source.version).join("+");
      const found = await this.#query(database => database
        .prepare(`SELECT id FROM ${this.#table} WHERE version = ? AND state IN ('pending', 'active')`)
        .get<Generation>([version]));
      const current = found && await this.#generation(found.id) ? found : undefined;
      if (found && !current) {
        await this.#query(database => database
          .prepare(`UPDATE ${this.#table} SET state = 'retired' WHERE id = ?`)
          .run([found.id]));
      }
      if (current) {
        await this.#query(database => database
          .prepare(`UPDATE ${this.#table} SET state = 'retired' WHERE state = 'pending' AND id != ?`)
          .run([current.id]));
      }
      await this.#discardStale();
      if (current) return;
      const { lastInsertRowid } = await this.#query(database => database
        .prepare(`INSERT INTO ${this.#table} (version, state) VALUES (?, 'staging')`)
        .run([version]));
      const root = `generation-${lastInsertRowid}`;
      for (const source of sources) {
        const name = `${this.#host.name} ${source.version}`.replace(/[<>:"/\\|?*]/g, "_");
        const archive = this.#host.cache.open(name);
        await this.#host.download(source.url, archive, { sha256: source.sha256, byteLength: source.size });
        const directory = this.#host.cache.directory(source.directory ? `${root}/${source.directory}` : root);
        await this.#host.unarchive(archive, directory, { stripRoot: true });
        await this.#host.cache.remove(name);
      }
      await this.#query(database => database.transactionAsync(async transaction => {
        await transaction.exec(`UPDATE ${this.#table} SET state = 'retired' WHERE state = 'pending'`);
        await transaction.prepare(`UPDATE ${this.#table} SET state = 'pending' WHERE id = ?`).run([lastInsertRowid]);
      }));
    });
  }

  /** The active generation, without promoting a pending one. */
  async active(): Promise<CacheDirectoryEntry<Entry> | undefined> {
    const active = await this.#query(database => database
      .prepare(`SELECT id FROM ${this.#table} WHERE state = 'active'`)
      .get<Generation>());
    return active && await this.#generation(active.id);
  }

  /** Promotes a pending generation and returns the generation to launch. */
  async launch(): Promise<CacheDirectoryEntry<Entry>> {
    const active = await this.#query(database => database.transactionAsync(async transaction => {
      const pending = await transaction
        .prepare(`SELECT id FROM ${this.#table} WHERE state = 'pending'`)
        .get<Generation>();
      if (pending) {
        await transaction.exec(`UPDATE ${this.#table} SET state = 'retired' WHERE state = 'previous'`);
        await transaction.exec(`UPDATE ${this.#table} SET state = 'previous' WHERE state = 'active'`);
        await transaction.prepare(`UPDATE ${this.#table} SET state = 'active' WHERE id = ?`).run([pending.id]);
      }
      return transaction
        .prepare(`SELECT id FROM ${this.#table} WHERE state = 'active'`)
        .get<Generation>();
    }));
    const generation = active && await this.#generation(active.id);
    if (!generation) throw new Error(`${this.#host.name} has not finished downloading`);
    return generation;
  }

  async #query<T>(work: (database: Database) => Promise<T>): Promise<T> {
    const database = await this.#host.connect();
    try {
      await database.exec(`CREATE TABLE IF NOT EXISTS ${this.#table} (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        version TEXT NOT NULL,
        state TEXT NOT NULL CHECK (state IN ('staging', 'pending', 'active', 'previous', 'retired'))
      )`);
      return await work(database);
    } finally { await database.close(); }
  }

  async #generation(id: number): Promise<CacheDirectoryEntry<Entry> | undefined> {
    for await (const entry of this.#host.cache) {
      if (EmulatorUpdates.#directory(entry) && entry.name === `generation-${id}`) return entry;
    }
  }

  static #directory<Entry extends CacheEntry>(entry: Entry): entry is CacheDirectoryEntry<Entry> {
    return entry.kind === "directory";
  }

  async #discardStale(): Promise<void> {
    const stale = await this.#query(database => database
      .prepare(`SELECT id FROM ${this.#table} WHERE state IN ('staging', 'retired')`)
      .all<Generation>());
    for (const { id } of stale) {
      try {
        await this.#host.cache.directory(`generation-${id}`).delete();
        await this.#query(database => database
          .prepare(`DELETE FROM ${this.#table} WHERE id = ?`)
          .run([id]));
      } catch (error) {
        console.warn(`${this.#host.name}: generation ${id} is still in use`, error);
      }
    }
  }
}
