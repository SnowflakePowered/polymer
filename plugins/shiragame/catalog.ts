import { connect } from "polymer:database";
import type { PluginCacheFile, PluginDatabase } from "polymer:api";
import type { Job } from "polymer:jobs";

export type Release = { shiragame: string; schema_version: string; consolespec_version: string; release: string };

export class ShiragameCatalog {
  static readonly INDEX_VERSION = 1;

  static async inspect(db: PluginDatabase): Promise<Release> {
    const rows = await db.prepare("SELECT shiragame, schema_version, consolespec_version, release FROM shiragame LIMIT 2").all<Release>();
    const release = rows[0];
    if (rows.length !== 1 || release.shiragame !== "shiragame" || !/^4\.\d+\.\d+$/.test(release.schema_version)
      || release.consolespec_version !== "0.0.1" || !release.release) {
      throw new Error("Unsupported Shiragame database schema (expected schema 4.x and consolespec 0.0.1)");
    }
    for (const sql of [
      "SELECT game_id, platform_id, entry_name, source FROM game LIMIT 0",
      "SELECT rom_id, game_id, file_name, sha1, md5, crc, size FROM rom LIMIT 0",
      "SELECT game_id, serial, normalized FROM serial LIMIT 0",
      "SELECT rom_id, contents FROM cue LIMIT 0",
    ]) await db.prepare(sql).all();
    return release;
  }

  static async build(file: PluginCacheFile, job: Job): Promise<Release> {
    const db = await connect(file, { readOnly: false });
    try {
      job.progress({ message: "Checking Shiragame database" });
      const release = await this.inspect(db);
      const checks = await db.prepare("PRAGMA quick_check").all();
      if (checks.length !== 1 || Object.values(checks[0])[0] !== "ok") throw new Error("Shiragame integrity check failed");
      const indexes = [
        "CREATE INDEX IF NOT EXISTS polymer_shiragame_sha1 ON rom(sha1)",
        "CREATE INDEX IF NOT EXISTS polymer_shiragame_md5 ON rom(md5)",
        "CREATE INDEX IF NOT EXISTS polymer_shiragame_crc ON rom(crc, size)",
        "CREATE INDEX IF NOT EXISTS polymer_shiragame_serial ON serial(serial)",
        "CREATE INDEX IF NOT EXISTS polymer_shiragame_normalized ON serial(normalized)",
      ];
      for (let i = 0; i < indexes.length; i++) {
        job.progress({ message: "Indexing Shiragame hashes and serials", completed: i, total: indexes.length });
        await db.exec(indexes[i]);
      }
      const checkpoint = await db.prepare("PRAGMA wal_checkpoint(TRUNCATE)").get<{ busy: number }>();
      if (checkpoint?.busy !== 0) throw new Error("Shiragame index checkpoint is busy");
      job.progress({ message: "Shiragame index ready", completed: indexes.length, total: indexes.length });
      return release;
    } finally {
      await db.close();
    }
  }
}
