import type { PluginDatabase, ScrapeInput, ScrapeResult } from "polymer:api";
import { Serial } from "pkg:serial.ts";

const MAX_MATCHES = 128;
const MAX_CUES = 256;
const hashes = [
  { algorithm: "sha1", column: "sha1" },
  { algorithm: "md5", column: "md5" },
  { algorithm: "crc32", column: "crc" },
] as const;
type HashColumn = typeof hashes[number]["column"];
type Dump = { game_id: number; sha1: string | null; md5: string | null; crc: string | null; size: number };
type Fingerprint = { digests: Partial<Record<HashColumn, string>>; size?: number };
type Cue = { file_name: string; contents: number[]; entry_name: string; source: string };

export class ShiragameMatcher {
  constructor(readonly db: PluginDatabase, readonly release: string) {}

  async *scrape(input: ScrapeInput): AsyncGenerator<ScrapeResult> {
    const platforms = new Set((input["package.platform"] ?? []).map(({ value }) => value));
    if (platforms.size !== 1) return;
    const platform = platforms.values().next().value;
    if (typeof platform !== "string" || !/^[A-Z0-9_]+$/.test(platform)) return;
    const hash = await this.matchHashes(input, platform);
    const games = hash.matched ? hash.games : await this.matchSerials(input, platform);
    for await (const cue of this.cues(games)) {
      yield {
        key: "media.cue",
        value: { kind: "inline", mediaType: "application/x-cue", bytes: cue.contents },
        source: `Shiragame ${this.release} / ${cue.source} / ${cue.entry_name} / ${cue.file_name}`,
      };
    }
  }

  private async *cues(games: Set<number>): AsyncGenerator<Cue> {
    const seen = new Set<string>();
    for await (const cue of this.cueRows(games)) {
      if (!Array.isArray(cue.contents) || !cue.contents.length) {
        throw new Error("Shiragame contains an invalid cue payload");
      }
      const identity = cue.contents.join(",");
      if (seen.has(identity)) continue;
      seen.add(identity);
      if (seen.size > MAX_CUES) throw new Error("Shiragame cue matches exceed the scrape limit");
      yield cue;
    }
  }

  private async *cueRows(games: Set<number>): AsyncGenerator<Cue> {
    for (const game of [...games].sort((a, b) => a - b)) {
      const cues = await this.db.prepare(`
        SELECT r.file_name, c.contents, g.entry_name, g.source
        FROM rom r JOIN cue c ON c.rom_id = r.rom_id JOIN game g ON g.game_id = r.game_id
        WHERE r.game_id = ? ORDER BY r.file_name, r.rom_id LIMIT ${MAX_CUES + 1}
      `).all<Cue>([game]);
      if (cues.length > MAX_CUES) throw new Error("Too many Shiragame cue rows for one game");
      yield* cues;
    }
  }

  async matchHashes(input: ScrapeInput, platform: string): Promise<{ games: Set<number>; matched: boolean }> {
    const games = new Set<number>();
    let matched = false;
    for await (const fingerprint of this.fingerprints(input)) {
      const rows = await this.lookup(fingerprint, platform);
      matched ||= rows.length > 0;
      const compatible = rows.filter(row => this.agrees(row, fingerprint));
      if (rows.length && !compatible.length) console.warn("Shiragame: conflicting dump hashes or size; ignoring serial fallback");
      for (const row of compatible) games.add(row.game_id);
      if (games.size > MAX_MATCHES) throw new Error("Too many Shiragame hash matches");
    }
    return { games, matched };
  }

  private async lookup(fingerprint: Fingerprint, platform: string): Promise<Dump[]> {
    for (const hash of hashes) {
      if (!fingerprint.digests[hash.column] || (hash.column === "crc" && fingerprint.size === undefined)) continue;
      // Column identifiers come exclusively from the static allowlist above.
      const rows = await this.db.prepare(`
        SELECT r.game_id, r.sha1, r.md5, r.crc, r.size
        FROM rom r JOIN game g ON g.game_id = r.game_id
        WHERE r.${hash.column} = ? AND g.platform_id = ?
        ${hash.column === "crc" ? "AND r.size = ?" : ""}
        LIMIT ${MAX_MATCHES + 1}
      `).all<Dump>([
        fingerprint.digests[hash.column], platform,
        ...(hash.column === "crc" ? [fingerprint.size] : []),
      ]);
      if (rows.length > MAX_MATCHES) throw new Error("Shiragame hash is too ambiguous");
      if (rows.length) return rows;
    }
    return [];
  }

  private agrees(row: Dump, fingerprint: Fingerprint): boolean {
    if (fingerprint.size !== undefined && row.size !== fingerprint.size) return false;
    return hashes.every(({ column }) => !fingerprint.digests[column] || !row[column]
      || row[column]?.toLowerCase() === fingerprint.digests[column]);
  }

  private async *fingerprints(input: ScrapeInput): AsyncGenerator<Fingerprint> {
    const files = input.packages.flatMap(package_ => package_.files);
    if (files.length > MAX_MATCHES) throw new Error("Too many dump fingerprints supplied to Shiragame");
    for (const file of files) {
      const digests: Fingerprint["digests"] = {};
      for (const hash of hashes) digests[hash.column] = await file.digest(hash.algorithm);
      yield { digests, size: file.size };
    }
  }

  async matchSerials(input: ScrapeInput, platform: string): Promise<Set<number>> {
    const games = new Set<number>();
    const serials = new Set((input["package.serial"] ?? [])
      .map(({ value }) => value).filter((value): value is string => typeof value === "string" && value.length > 0 && value.length <= 256));
    if (serials.size > MAX_MATCHES) throw new Error("Too many serials supplied to Shiragame");
    for (const serial of serials) {
      const variants = new Set([serial, serial.trim(), serial.trim().toUpperCase()]);
      for (const variant of variants) {
        const pairs = [["serial", variant], ["normalized", Serial.normalize(platform, variant)]] as const;
        for (const [column, value] of pairs) {
          const rows = await this.db.prepare(`
            SELECT DISTINCT g.game_id FROM serial s JOIN game g ON g.game_id = s.game_id
            WHERE s.${column} = ? AND g.platform_id = ? LIMIT ${MAX_MATCHES + 1}
          `).all<{ game_id: number }>([value, platform]);
          for (const row of rows) games.add(row.game_id);
          if (games.size > MAX_MATCHES) throw new Error("Shiragame serial is too ambiguous");
        }
      }
    }
    return games;
  }
}
