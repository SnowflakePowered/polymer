import { Reader, ZipReader } from "jsr:@zip-js/zip-js@2.14.0";
import { caches } from "polymer:cache";
import type { PluginCacheFile, PluginProgressContext } from "polymer:api";

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
  async download(path: string, context: PluginProgressContext): Promise<void> {
    context.progress({ message: "Downloading Shiragame database" });
    const response = await fetch(RELEASE_URL);
    if (!response.ok || !response.body) throw new Error(`Shiragame download failed: HTTP ${response.status}`);
    const length = Number(response.headers.get("content-length"));
    if (length > MAX_ARCHIVE_BYTES) {
      await response.body.cancel();
      throw new Error("Shiragame archive exceeds the download limit");
    }
    const file = caches.download.open(path, { read: false, write: true, truncate: true });
    if (!file.writable) throw new Error("Shiragame download cache is not writable");
    await response.body.pipeThrough(this.progress(context, "Downloading Shiragame database", MAX_ARCHIVE_BYTES, length))
      .pipeTo(file.writable);
  }

  async extract(archive: string, path: string, context: PluginProgressContext): Promise<void> {
    const zip = new ZipReader(new CacheReader(caches.download.open(archive, { read: false })), { useWebWorkers: false });
    try {
      const entries = (await zip.getEntries()).filter(entry => entry.filename === "shiragame.db" && !entry.directory);
      const entry = entries[0];
      if (entries.length !== 1 || !entry || !("getData" in entry) || !entry.getData) throw new Error("Shiragame archive must contain one shiragame.db");
      if (entry.uncompressedSize <= 0 || entry.uncompressedSize > MAX_DATABASE_BYTES) throw new Error("Shiragame database exceeds the extraction limit");
      const file = caches.download.open(path, { read: false, write: true, truncate: true });
      if (!file.writable) throw new Error("Shiragame database cache is not writable");
      const progress = this.progress(context, "Extracting Shiragame database", MAX_DATABASE_BYTES, entry.uncompressedSize);
      const abort = new AbortController();
      const writing = progress.readable.pipeTo(file.writable, { signal: abort.signal });
      try {
        await Promise.all([entry.getData(progress.writable, { checkSignature: true }), writing]);
      } catch (error) {
        abort.abort(error);
        await Promise.allSettled([writing]);
        throw error;
      }
      if (file.size !== entry.uncompressedSize) throw new Error("Shiragame database extraction was incomplete");
    } finally { await zip.close(); }
  }

  private progress(context: PluginProgressContext, message: string, maximum: number, total: number): TransformStream<Uint8Array, Uint8Array> {
    let completed = 0;
    let reported = 0;
    context.progress({ message, completed: 0, total: total > 0 ? total : undefined });
    return new TransformStream({
      transform(chunk, controller) {
        completed += chunk.byteLength;
        if (completed > maximum) throw new Error(`${message} exceeds the byte limit`);
        if (completed - reported >= 4 * 1024 * 1024) {
          context.progress({ message, completed, total: total > 0 ? Math.max(total, completed) : undefined });
          reported = completed;
        }
        controller.enqueue(chunk);
      },
      flush() { context.progress({ message, completed, total: total > 0 ? Math.max(total, completed) : undefined }); },
    });
  }
}
