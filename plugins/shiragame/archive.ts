import { caches } from "polymer:plugin";
import { download } from "polymer:download";
import { unarchive } from "polymer:unarchive";

export const RELEASE_URL = "https://github.com/SnowflakePowered/shiragame/releases/latest/download/shiragame.db.zip";
const MAX_ARCHIVE_BYTES = 256 * 1024 * 1024;
const MAX_DATABASE_BYTES = 1024 * 1024 * 1024;

export class ShiragameArchive {
  async download(path: string): Promise<void> {
    await download(RELEASE_URL, caches.download.open(path), { maxByteLength: MAX_ARCHIVE_BYTES });
  }

  /** Extracts the archive into a directory and returns the path of its `shiragame.db`. */
  async extract(archive: string, directory: string): Promise<string> {
    await unarchive(caches.download.open(archive, { read: false }), caches.download.directory(directory));
    const path = `${directory}/shiragame.db`;
    let size: number;
    try { size = caches.download.open(path, { read: false }).size; }
    catch { throw new Error("Shiragame archive must contain shiragame.db"); }
    if (size <= 0 || size > MAX_DATABASE_BYTES) throw new Error("Shiragame database exceeds the extraction limit");
    return path;
  }
}
