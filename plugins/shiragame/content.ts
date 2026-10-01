import { caches } from "polymer:cache";
import { connect } from "polymer:database";
import { Job } from "polymer:jobs";
import { ShiragameCatalog } from "pkg:catalog.ts";
import { ShiragameArchive } from "pkg:archive.ts";

export type Generation = { path: string; release: string; index_version: number };

export class ShiragameContent {
  /** Whether the active generation is indexed for this version and matches its database. */
  async ready(): Promise<boolean> {
    const state = await connect();
    try {
      await state.exec(`
        CREATE TABLE IF NOT EXISTS shiragame_generations (id INTEGER PRIMARY KEY AUTOINCREMENT);
        CREATE TABLE IF NOT EXISTS shiragame_active (
          id INTEGER PRIMARY KEY CHECK (id = 1), path TEXT NOT NULL,
          release TEXT NOT NULL, index_version INTEGER NOT NULL
        );
      `);
    } finally { await state.close(); }
    const current = await this.current();
    if (current?.index_version === ShiragameCatalog.INDEX_VERSION) {
      try {
        const db = await connect(caches.download.open(current.path, { read: false }));
        try {
          const release = await ShiragameCatalog.inspect(db);
          if (release.release === current.release) return true;
        } finally { await db.close(); }
      } catch (error) { console.warn("Shiragame: rebuilding unavailable cache", error); }
    }
    return false;
  }

  async update(job: Job): Promise<void> {
    const staged = await this.ready() ? undefined : await this.staged();
    if (staged) await this.publish(staged, job);
    else await this.refresh(job);
  }

  private async staged(): Promise<string | undefined> {
    const state = await connect();
    try {
      const latest = await state.prepare("SELECT MAX(id) AS id FROM shiragame_generations").get<{ id: number | null }>();
      if (!latest?.id) return;
      const path = `generation-${latest.id}.db`;
      // A previous boot may have stopped during indexing. Reuse its extracted
      // database; CREATE INDEX IF NOT EXISTS preserves already completed work.
      const db = await connect(caches.download.open(path, { read: false }));
      try { await ShiragameCatalog.inspect(db); }
      finally { await db.close(); }
      return path;
    } catch (error) {
      console.warn("Shiragame: no reusable staged database", error);
    } finally { await state.close(); }
  }

  async current(): Promise<Generation | undefined> {
    const state = await connect();
    try {
      return await state.prepare("SELECT path, release, index_version FROM shiragame_active WHERE id = 1").get<Generation>();
    } finally { await state.close(); }
  }

  async refresh(job: Job): Promise<void> {
    const state = await connect();
    let generation: number;
    try {
      const { lastInsertRowid } = await state.prepare("INSERT INTO shiragame_generations DEFAULT VALUES").run();
      generation = Number(lastInsertRowid);
    } finally { await state.close(); }
    const archivePath = `generation-${generation}.zip`;
    const path = `generation-${generation}.db`;
    const archive = new ShiragameArchive();
    await archive.download(archivePath);
    await archive.extract(archivePath, path);
    await this.publish(path, job);
  }

  private async publish(path: string, job: Job): Promise<void> {
    const release = await ShiragameCatalog.build(caches.download.open(path, { read: false }), job);
    const state = await connect();
    try {
      // Publish only after validation, indexing, checkpointing, and closing the
      // staged database. Existing readers keep their immutable generation.
      await state.prepare("INSERT OR REPLACE INTO shiragame_active (id, path, release, index_version) VALUES (1, ?, ?, ?)")
        .run([path, release.release, ShiragameCatalog.INDEX_VERSION]);
      await state.exec("PRAGMA wal_checkpoint(TRUNCATE)");
      job.progress({ message: "Shiragame cue sheets ready" });
    } finally { await state.close(); }
  }
}
