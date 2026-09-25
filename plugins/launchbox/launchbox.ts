import { IndexWriter } from "pkg:index-writer.ts";
import { parseXmlStreamFromBytes } from "jsr:@std/xml/parse-stream";
import { Reader, ZipReader } from "jsr:@zip-js/zip-js";
import { caches } from "polymer:cache";
import { connect } from "polymer:database";
import type { OnPluginLoad, UpdateContents, PluginProgressContext, OnScrape, ScrapeCandidate, ScrapeInput, ScrapeResult } from "polymer:api";

const SOURCE = "LaunchBox Games Database";
const METADATA_URL = "https://gamesdb.launchbox-app.com/Metadata.zip";
const IMAGE_ROOT = "https://images.launchbox-app.com/";
const DOWNLOAD_FILE = "Metadata.zip";

const platformNames: Readonly<Record<string, string>> = Object.freeze({
  ATARI_2600: "Atari 2600", ATARI_5200: "Atari 5200", ATARI_7800: "Atari 7800",
  ATARI_LYNX: "Atari Lynx", ATARI_JAGUAR: "Atari Jaguar", BANDAI_WS: "WonderSwan",
  BANDAI_WSC: "WonderSwan Color", COLECO_CV: "ColecoVision", MATTEL_INTELLIVISION: "Intellivision",
  GCE_VECTREX: "GCE Vectrex", MICROSOFT_XBOX: "Microsoft Xbox", MICROSOFT_XBOX360: "Microsoft Xbox 360",
  NEC_TG16: "TurboGrafx-16", NINTENDO_GB: "Nintendo Game Boy", NINTENDO_GBC: "Nintendo Game Boy Color",
  NINTENDO_GBA: "Nintendo Game Boy Advance", NINTENDO_DS: "Nintendo DS", NINTENDO_DSI: "Nintendo DSi",
  NINTENDO_3DS: "Nintendo 3DS", NINTENDO_NES: "Nintendo Entertainment System",
  NINTENDO_FDS: "Nintendo Famicom Disk System", NINTENDO_SNES: "Super Nintendo Entertainment System",
  NINTENDO_N64: "Nintendo 64", NINTENDO_N64DD: "Nintendo 64DD", NINTENDO_VB: "Nintendo Virtual Boy",
  NINTENDO_GCN: "Nintendo GameCube", NINTENDO_WII: "Nintendo Wii", NINTENDO_WIIU: "Nintendo Wii U",
  NINTENDO_SWITCH: "Nintendo Switch", SEGA_GG: "Sega Game Gear", SEGA_SMS: "Sega Master System",
  SEGA_SG1000: "Sega SG-1000", SEGA_GEN: "Sega Genesis", SEGA_CD: "Sega CD", SEGA_32X: "Sega 32X",
  SEGA_SAT: "Sega Saturn", SEGA_DC: "Sega Dreamcast", SONY_PSX: "Sony Playstation",
  SONY_PS2: "Sony Playstation 2", SONY_PS3: "Sony Playstation 3", SONY_PSP: "Sony PSP",
  SONY_PSVITA: "Sony Playstation Vita", PHILIPS_CDI: "Philips CD-i", SNK_NGP: "Neo Geo Pocket",
  SNK_NGPC: "Neo Geo Pocket Color", SNK_NGCD: "Neo Geo CD", PANASONIC_3DO: "3DO Interactive Multiplayer",
});

type GameRecord = {
  id: number;
  platform: string;
  name: string;
  overview?: string;
  publisher?: string;
  genres?: string;
  esrb?: string;
  releaseDate?: string;
  releaseYear?: string;
  aliases?: string;
  matchRegion?: string;
};

type ImageRecord = { id: number; fileName: string; type: string; region: string };
type AlternateRecord = { id: number; name: string; region: string };
type FileRecord = { platform: string; fileName: string; gameName: string };

class CacheReader extends Reader<ReturnType<typeof caches.download.open>> {
  readonly file: ReturnType<typeof caches.download.open>;

  constructor(file: ReturnType<typeof caches.download.open>) {
    super(file);
    this.file = file;
    this.size = file.size;
  }

  readUint8Array(index: number, length: number): Promise<Uint8Array> {
    return this.file.read(index, length);
  }
}

function normalize(value: string): string {
  return value.normalize("NFKD").toLocaleLowerCase("en-US")
    .replace(/&/g, " and ").replace(/[^\p{L}\p{N}]+/gu, " ").trim().replace(/\s+/g, " ");
}

function titleWithoutTags(value: string): string {
  return normalize(value.replace(/\s*\([^)]*\)/g, " ").replace(/\s*\[[^\]]*\]/g, " "));
}

function region(value: string): string {
  const match = value.match(/\(([^)]*(?:USA|Europe|Japan|World|Korea|Australia)[^)]*)\)/i);
  return match ? normalize(match[1]) : "";
}

function values(input: ScrapeInput, keys: readonly `${string}.${string}`[]): ScrapeCandidate[] {
  return keys.flatMap((key) => input[key] ?? []);
}

function strings(input: ScrapeInput, keys: readonly `${string}.${string}`[]): string[] {
  return values(input, keys).map(({ value }) => value).filter((value): value is string => typeof value === "string");
}

async function* streamChunks(stream: ReadableStream<Uint8Array>, flush: () => Promise<void>, progress: (bytes: number) => void): AsyncGenerator<Uint8Array> {
  const reader = stream.getReader();
  let bytes = 0;
  let reported = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) { progress(bytes); return; }
      yield value;
      await flush();
      bytes += value.byteLength;
      if (bytes - reported >= 1024 * 1024) { progress(bytes); reported = bytes; }
    }
  } finally {
    await reader.cancel();
    reader.releaseLock();
  }
}

async function buildIndex(context?: PluginProgressContext): Promise<void> {
  const db = await connect();
  await db.exec(`
    DROP TABLE IF EXISTS games_next;
    DROP TABLE IF EXISTS images_next;
    DROP TABLE IF EXISTS aliases_next;
    DROP TABLE IF EXISTS raw_aliases_next;
    CREATE TABLE games_next (
      id INTEGER PRIMARY KEY, platform TEXT NOT NULL, name TEXT NOT NULL,
      normalized TEXT NOT NULL, overview TEXT, publisher TEXT, genres TEXT,
      esrb TEXT, release_date TEXT
    );
    CREATE TABLE images_next (
      game_id INTEGER NOT NULL, key TEXT NOT NULL, url TEXT NOT NULL, ordinal INTEGER NOT NULL, region TEXT NOT NULL,
      PRIMARY KEY(game_id, key, ordinal)
    );
    CREATE TABLE aliases_next (
      game_id INTEGER NOT NULL, normalized TEXT NOT NULL, region TEXT NOT NULL,
      PRIMARY KEY(game_id, normalized, region)
    );
    CREATE TABLE raw_aliases_next (
      platform TEXT NOT NULL, normalized TEXT NOT NULL,
      game_normalized TEXT NOT NULL, region TEXT NOT NULL
    );
    BEGIN IMMEDIATE;
  `);
  const games = new IndexWriter(db, "INSERT OR REPLACE INTO games_next", "games");
  const images = new IndexWriter(db, "INSERT OR IGNORE INTO images_next", "images");
  const aliases = new IndexWriter(db, "INSERT INTO raw_aliases_next", "aliases");
  const alternates = new IndexWriter(db, "INSERT OR IGNORE INTO aliases_next", "alternate names");
  console.info("LaunchBox: building metadata index");
  let record: GameRecord | ImageRecord | AlternateRecord | undefined;
  let recordType: "Game" | "GameImage" | "GameAlternateName" | undefined;
  let field = "";
  let text = "";
  const imageOrdinals = new Map<string, number>();
  let zip: ZipReader<unknown> | undefined;
  try {
    const archiveFile = caches.download.open(DOWNLOAD_FILE);
    zip = new ZipReader(new CacheReader(archiveFile), { useWebWorkers: false });
    const entries = await zip.getEntries();
    const entry = entries.find((entry) => entry.filename === "Metadata.xml");
    if (!entry?.getData) throw new Error("LaunchBox archive does not contain Metadata.xml");
    context?.progress({ message: "Indexing game metadata", completed: 0, total: entry.uncompressedSize });
    const xml = new TransformStream<Uint8Array, Uint8Array>();
    const extraction = entry.getData(xml.writable);
    await Promise.all([extraction, parseXmlStreamFromBytes(streamChunks(xml.readable, async () => { await games.flush(); await images.flush(); await alternates.flush(); }, completed => context?.progress({ message: "Indexing game metadata", completed, total: entry.uncompressedSize })), {
      onStartElement(name) {
        if (name === "Game") { recordType = "Game"; record = { id: 0, platform: "", name: "" }; }
        else if (name === "GameImage") { recordType = "GameImage"; record = { id: 0, fileName: "", type: "", region: "" }; }
        else if (name === "GameAlternateName") { recordType = "GameAlternateName"; record = { id: 0, name: "", region: "" }; }
        else if (record) { field = name; text = ""; }
      },
      onText(value) { if (record && field) text += value; },
      onEndElement(name) {
        if (record && field === name) {
          const value = text.trim();
          if (recordType === "Game") {
            const game = record as GameRecord;
            if (name === "DatabaseID") game.id = Number(value);
            else if (name === "Platform") game.platform = value;
            else if (name === "Name") game.name = value;
            else if (name === "Overview") game.overview = value;
            else if (name === "Publisher") game.publisher = value;
            else if (name === "Genres") game.genres = value;
            else if (name === "ESRB") game.esrb = value;
            else if (name === "ReleaseDate") game.releaseDate = value.slice(0, 10);
            else if (name === "ReleaseYear") game.releaseYear = value;
          } else if (recordType === "GameAlternateName") {
            const alternate = record as AlternateRecord;
            if (name === "DatabaseID") alternate.id = Number(value);
            else if (name === "AlternateName") alternate.name = value;
            else if (name === "Region") alternate.region = value;
          } else {
            const image = record as ImageRecord;
            if (name === "DatabaseID") image.id = Number(value);
            else if (name === "FileName") image.fileName = value;
            else if (name === "Type") image.type = value;
            else if (name === "Region") image.region = value;
          }
          field = ""; text = "";
        }
        if (name === recordType && record) {
          if (recordType === "Game") {
            const game = record as GameRecord;
            if (game.id && game.platform && game.name) {
              games.add([game.id, game.platform, game.name, normalize(game.name), game.overview ?? null, game.publisher ?? null, game.genres ?? null, game.esrb ?? null, game.releaseDate ?? (game.releaseYear ? `${game.releaseYear}-01-01` : null)]);

            }
          } else if (recordType === "GameAlternateName") {
            const alternate = record as AlternateRecord;
            if (alternate.id && alternate.name) alternates.add([alternate.id, normalize(alternate.name), normalize(alternate.region)]);
          } else {
            const image = record as ImageRecord;
            const key = imageKey(image.type);
            if (image.id && image.fileName && key) {
              const ordinalKey = `${image.id}:${key}`;
              const ordinal = imageOrdinals.get(ordinalKey) ?? 0;
              imageOrdinals.set(ordinalKey, ordinal + 1);
              images.add([image.id, key, `${IMAGE_ROOT}${encodeURIComponent(image.fileName)}`, ordinal, image.region]);

            }
          }
          record = undefined; recordType = undefined; field = ""; text = "";
        }
      },
    })]);
    const filesEntry = entries.find((entry) => entry.filename === "Files.xml");
    if (!filesEntry?.getData) throw new Error("LaunchBox archive does not contain Files.xml");
    context?.progress({ message: "Indexing filenames", completed: 0, total: filesEntry.uncompressedSize });
    const files = new TransformStream<Uint8Array, Uint8Array>();
    const filesExtraction = filesEntry.getData(files.writable);
    let file: FileRecord | undefined;
    field = "";
    text = "";
    await Promise.all([filesExtraction, parseXmlStreamFromBytes(streamChunks(files.readable, () => aliases.flush(), completed => context?.progress({ message: "Indexing filenames", completed, total: filesEntry.uncompressedSize })), {
      onStartElement(name) {
        if (name === "File") file = { platform: "", fileName: "", gameName: "" };
        else if (file) { field = name; text = ""; }
      },
      onText(value) { if (file && field) text += value; },
      onEndElement(name) {
        if (file && field === name) {
          const value = text.trim();
          if (name === "Platform") file.platform = value;
          else if (name === "FileName") file.fileName = value;
          else if (name === "GameName") file.gameName = value;
          field = "";
          text = "";
        }
        if (name === "File" && file) {
          if (file.platform && file.fileName && file.gameName) {
            aliases.add([
              file.platform,
              titleWithoutTags(file.fileName),
              titleWithoutTags(file.gameName),
              region(file.fileName),
            ]);

          }
          file = undefined;
        }
      },
    })]);
    await games.flush();
    await images.flush();
    await alternates.flush();
    await aliases.flush();
    context?.progress({ message: "Matching aliases and publishing metadata index" });
    console.info("LaunchBox: matching aliases and publishing metadata index");
    await db.exec(`
      CREATE INDEX games_next_match ON games_next(platform, normalized);
      COMMIT;
      INSERT OR IGNORE INTO aliases_next (game_id, normalized, region)
        SELECT games_next.id, raw_aliases_next.normalized, raw_aliases_next.region
        FROM raw_aliases_next
        JOIN games_next ON games_next.platform = raw_aliases_next.platform
          AND games_next.normalized = raw_aliases_next.game_normalized;
      DROP TABLE raw_aliases_next;
      BEGIN IMMEDIATE;
      DROP TABLE IF EXISTS games;
      DROP TABLE IF EXISTS images;
      DROP TABLE IF EXISTS aliases;
      ALTER TABLE games_next RENAME TO games;
      ALTER TABLE images_next RENAME TO images;
      ALTER TABLE aliases_next RENAME TO aliases;
      DROP INDEX games_next_match;
      CREATE INDEX games_match ON games(platform, normalized);
      CREATE INDEX images_game ON images(game_id, key, ordinal);
      CREATE INDEX aliases_match ON aliases(normalized, region, game_id);
      CREATE TABLE IF NOT EXISTS content_state (key TEXT PRIMARY KEY, value TEXT NOT NULL);
      INSERT OR REPLACE INTO content_state VALUES ('ready', '3');
      COMMIT;
    `);
    console.info("LaunchBox: metadata index ready");
  } catch (error) {
    try { await db.exec("ROLLBACK"); } catch { /* transaction was already closed */ }
    throw error;
  } finally {
    await zip?.close();
  }
}

function imageKey(type: string): string | undefined {
  if (type === "Box - Front" || type === "Fanart - Box - Front") return "media.boxart.front";
  if (type === "Box - Back" || type === "Fanart - Box - Back") return "media.boxart.back";
  if (type === "Box - Spine") return "media.boxart.spine";
  if (type === "Box - Full") return "media.boxart.full";
  if (type === "Clear Logo") return "media.logo";
  if (type === "Banner") return "media.banner";
  if (type.startsWith("Disc")) return "media.label.disc";
  if (type.startsWith("Cart - Front")) return "media.label.cart";
  return undefined;
}

async function downloadAndBuildIndex(context?: PluginProgressContext): Promise<void> {
  context?.progress({ message: "Downloading LaunchBox metadata" });
  const response = await fetch(METADATA_URL);
  if (!response.ok || !response.body) throw new Error(`LaunchBox metadata download failed: ${response.status}`);
  const file = caches.download.open(DOWNLOAD_FILE, { read: false, write: true, create: true, truncate: true });
  if (!file.writable) throw new Error("LaunchBox download cache is not writable");
  const length = Number(response.headers.get("content-length"));
  let downloaded = 0;
  let reported = 0;
  const downloadProgress = new TransformStream<Uint8Array, Uint8Array>({
    transform(chunk, controller) {
      downloaded += chunk.byteLength;
      if (downloaded - reported >= 1024 * 1024) {
        context?.progress({ message: "Downloading LaunchBox metadata", completed: downloaded, total: length > 0 ? Math.max(length, downloaded) : undefined });
        reported = downloaded;
      }
      controller.enqueue(chunk);
    },
  });
  await response.body.pipeThrough(downloadProgress).pipeTo(file.writable as WritableStream<Uint8Array>);
  await buildIndex(context);
}

let updatedDuringLoad = false;

export const updateContents: UpdateContents = async (context) => {
  if (updatedDuringLoad) {
    updatedDuringLoad = false;
    return;
  }
  await downloadAndBuildIndex(context);
};

export const onPluginLoad: OnPluginLoad = async (context) => {
  const db = await connect();
  let version: string | undefined;
  try {
    version = (await db.prepare("SELECT value FROM content_state WHERE key = 'ready'").get<{ value: string }>())?.value;
  } catch { /* first use has no schema */ }
  if (version === "3") {
    // Startup requests an update after loading the plugin. The persistent index
    // satisfies that first request even when it was built by an earlier process.
    updatedDuringLoad = true;
    return;
  }
  let cached = false;
  try { cached = caches.download.open(DOWNLOAD_FILE).size > 0; } catch { /* no cached archive */ }
  if (cached) await buildIndex(context);
  else await downloadAndBuildIndex(context);
  updatedDuringLoad = true;
};

function jaroWinkler(left: string, right: string): number {
  if (left === right) return 1;
  if (!left.length || !right.length) return 0;
  const distance = Math.max(Math.floor(Math.max(left.length, right.length) / 2) - 1, 0);
  const leftMatch = new Array(left.length).fill(false);
  const rightMatch = new Array(right.length).fill(false);
  let matches = 0;
  for (let i = 0; i < left.length; i++) for (let j = Math.max(0, i - distance); j <= Math.min(i + distance, right.length - 1); j++) {
    if (!rightMatch[j] && left[i] === right[j]) { leftMatch[i] = true; rightMatch[j] = true; matches++; break; }
  }
  if (!matches) return 0;
  const a = leftMatch.flatMap((match, index) => match ? [left[index]] : []);
  const b = rightMatch.flatMap((match, index) => match ? [right[index]] : []);
  let transpositions = 0;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) transpositions++;
  const jaro = (matches / left.length + matches / right.length + (matches - transpositions / 2) / matches) / 3;
  let prefix = 0;
  while (prefix < 4 && prefix < left.length && left[prefix] === right[prefix]) prefix++;
  return jaro + prefix * 0.1 * (1 - jaro);
}

async function match(input: ScrapeInput): Promise<GameRecord | undefined> {
  const platformId = strings(input, ["package.platform"])[0];
  const platform = platformId ? platformNames[platformId] : undefined;
  if (!platform) return undefined;
  const requested = [...new Set(strings(input, ["meta.title", "meta.title.en", "filename.nointro.title", "filename.tosec.title", "filename.goodtools.title", "package.internalname"]).map(normalize).filter(Boolean))];
  if (!requested.length) return undefined;
  const requestedRegions = strings(input, ["filename.nointro.region", "filename.tosec.region", "filename.goodtools.region"])
    .flatMap((value) => normalize(value).split(" ")).filter(Boolean);
  const db = await connect();
  const projection = "id, platform, name, overview, publisher, genres, esrb, release_date AS releaseDate";
  const exact = db.prepare(`
    SELECT ${projection}, '' AS matchRegion FROM games WHERE platform = ? AND normalized = ?
    UNION ALL
    SELECT games.id, games.platform, games.name, games.overview, games.publisher,
           games.genres, games.esrb, games.release_date AS releaseDate, aliases.region AS matchRegion
      FROM aliases JOIN games ON games.id = aliases.game_id
      WHERE games.platform = ? AND aliases.normalized = ?
    ORDER BY id
  `);
  for (const title of requested) {
    const rows = await exact.all<GameRecord>([platform, title, platform, title]);
    const unique = [...new Map(rows.map((game) => [game.id, game])).values()];
    if (unique.length === 1) return unique[0];
    const regional = [...new Map(rows.filter((game) => {
      const candidate = normalize(game.matchRegion ?? game.name);
      return requestedRegions.some((value) => candidate.includes(value));
    }).map((game) => [game.id, game])).values()];
    if (regional.length === 1) return regional[0];
  }
  const candidates = await db.prepare(`
    SELECT ${projection}, GROUP_CONCAT(aliases.normalized, '|') AS aliases
      FROM games LEFT JOIN aliases ON aliases.game_id = games.id
      WHERE games.platform = ? GROUP BY games.id ORDER BY games.id
  `).all<GameRecord>([platform]);
  const ranked = candidates.map((game) => {
    const names = [normalize(game.name), ...(game.aliases?.split("|") ?? [])];
    return { game, score: Math.max(...requested.flatMap((title) => names.map((name) => jaroWinkler(title, name)))) };
  }).sort((a, b) => b.score - a.score || a.game.id - b.game.id);
  if (!ranked.length || ranked[0].score < 0.92 || (ranked[1] && ranked[0].score - ranked[1].score < 0.03)) return undefined;
  return ranked[0].game;
}

class ArtworkRegions {
  static canonical(value: string): string {
    const name = normalize(value);
    const aliases: Record<string, string> = {
      usa: "north america", us: "north america", "united states": "north america", canada: "north america",
      japan: "japan", jp: "japan", europe: "europe", eur: "europe",
      australia: "australia", aus: "australia", au: "australia", korea: "korea", southkorea: "korea", kr: "korea",
      eu: "europe", as: "asia", cn: "china", tw: "taiwan", hk: "hong kong", de: "germany", fr: "france",
      es: "spain", it: "italy", gb: "united kingdom", ru: "russia", br: "brazil", nl: "the netherlands",
      ca: "north america", se: "sweden", no: "norway", fi: "finland", gr: "greece",
    };
    return aliases[name] ?? name;
  }

  static select<T extends { key: string; region: string }>(images: T[], input: ScrapeInput): T[] {
    const catalogued = strings(input, ["meta.region"]).flatMap(value => value.split("-"));
    const requested = (catalogued.length ? catalogued : strings(input, ["filename.nointro.region", "filename.tosec.region", "filename.goodtools.region"]))
      .flatMap(value => value.split(/[,;]/)).map(value => this.canonical(value));
    if (!requested.length || requested.includes("world")) return images;
    const rank = (image: T) => {
      const region = this.canonical(image.region);
      if (requested.includes(region)) return 0;
      return !region || region === "world" ? 1 : 2;
    };
    const best = new Map<string, number>();
    for (const image of images) best.set(image.key, Math.min(best.get(image.key) ?? 2, rank(image)));
    return images.filter(image => rank(image) < 2 && rank(image) === best.get(image.key));
  }
}

export const onScrape: OnScrape = async function* (input): AsyncGenerator<ScrapeResult, void, void> {
  const game = await match(input);
  if (!game) return;
  yield { key: "meta.title", value: game.name, source: SOURCE };
  if (game.overview) yield { key: "meta.description", value: game.overview, source: SOURCE };
  if (game.publisher) yield { key: "meta.publisher", value: game.publisher, source: SOURCE };
  if (game.esrb) yield { key: "meta.rating.esrb", value: game.esrb, source: SOURCE };
  if (game.releaseDate) yield { key: "meta.releasedate", value: game.releaseDate, source: SOURCE };
  for (const genre of game.genres?.split(/[;,]/).map((value) => value.trim()).filter(Boolean) ?? []) yield { key: "meta.genre", value: genre, source: SOURCE };
  const db = await connect();
  const images = await db.prepare("SELECT key, url, region FROM images WHERE game_id = ? ORDER BY key, ordinal").all<{ key: string; url: string; region: string }>([game.id]);
  for (const image of ArtworkRegions.select(images, input)) yield { key: image.key, value: image.url, source: SOURCE };
};

export default { onPluginLoad, updateContents, onScrape };
