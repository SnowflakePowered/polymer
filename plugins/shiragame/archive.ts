import { Reader, ZipReader } from "jsr:@zip-js/zip-js@2.14.0";
import { caches } from "polymer:cache";
import { download } from "polymer:download";
import type { PluginCacheFile } from "polymer:api";
import { JobTrackedStream } from "polymer:jobs";

export const RELEASE_URL = "https://github.com/SnowflakePowered/shiragame/releases/latest/download/shiragame.db.zip";
const MAX_ARCHIVE_BYTES = 256 * 1024 * 1024;
const MAX_DATABASE_BYTES = 1024 * 1024 * 1024;

class CacheReader extends Reader<PluginCacheFile> {
  readonly file: PluginCacheFile;
  constructor(file: PluginCacheFile) {
    super(file);
    this.file = file;
    this.size = file.size;
  }
  readUint8Array(index: number, length: number): Promise<Uint8Array> {
    return this.file.read(index, length);
  }
}

export class ShiragameArchive {
  async download(path: string): Promise<void> {
    await download(RELEASE_URL, caches.download.open(path), { maxByteLength: MAX_ARCHIVE_BYTES });
  }

  async extract(archive: string, path: string): Promise<void> {
    const zip = new ZipReader(new CacheReader(caches.download.open(archive, { read: false })), { useWebWorkers: false });
    try {
      const entries = (await zip.getEntries()).filter(entry => entry.filename === "shiragame.db" && !entry.directory);
      const entry = entries[0];
      if (entries.length !== 1 || !entry || !("getData" in entry) || !entry.getData) throw new Error("Shiragame archive must contain one shiragame.db");
      if (entry.uncompressedSize <= 0 || entry.uncompressedSize > MAX_DATABASE_BYTES) throw new Error("Shiragame database exceeds the extraction limit");
      const file = caches.download.open(path, { read: false, write: true, truncate: true });
      if (!file.writable) throw new Error("Shiragame database cache is not writable");
      const extracted = new TransformStream<Uint8Array, Uint8Array>();
      const tracked = new JobTrackedStream(extracted.readable, {
        message: "Extracting Shiragame database",
        total: entry.uncompressedSize,
        maxByteLength: MAX_DATABASE_BYTES,
      });
      const abort = new AbortController();
      const writing = tracked.pipeTo(file.writable, { signal: abort.signal });
      try {
        await Promise.all([entry.getData(extracted.writable, { checkSignature: true }), writing]);
      } catch (error) {
        abort.abort(error);
        await Promise.allSettled([writing]);
        throw error;
      }
      if (file.size !== entry.uncompressedSize) throw new Error("Shiragame database extraction was incomplete");
    } finally { await zip.close(); }
  }
}
