import {
  op_cache_create,
  op_cache_keys,
  op_cache_read,
  op_cache_read_range,
  op_cache_size,
  op_cache_write,
} from "ext:core/ops";

const CHUNK_SIZE = 256 * 1024;

function plainPath(path) {
  path = String(path);
  if (!path || path.startsWith("/") || path.startsWith("\\") || path.split(/[\\/]/).some(part => !part || part === "." || part === "..")) {
    throw new TypeError("cache paths must contain only plain relative components");
  }
  return path;
}

function file(key, path, options) {
  let offset = 0;
  const readable = options.read === false ? undefined : new ReadableStream({
    pull(controller) {
      const chunk = op_cache_read_range(key, path, offset, CHUNK_SIZE);
      if (!chunk.length) {
        controller.close();
        return;
      }
      offset += chunk.length;
      controller.enqueue(chunk);
    },
  });
  let first = true;
  const writable = options.write !== true ? undefined : new WritableStream({
    start() { op_cache_create(key, path, options.truncate !== false); },
    write(chunk) {
      if (!(chunk instanceof Uint8Array)) chunk = new TextEncoder().encode(String(chunk));
      op_cache_write(key, path, chunk, !first || options.truncate === false);
      first = false;
    },
  });
  return Object.freeze({
    readable,
    writable,
    get size() { return op_cache_size(key, path); },
    async read(offset, length) {
      offset = Number(offset);
      length = Number(length);
      if (!Number.isSafeInteger(offset) || offset < 0 || !Number.isSafeInteger(length) || length < 0 || length > 0xffffffff) {
        throw new TypeError("cache read range is invalid");
      }
      return op_cache_read_range(key, path, offset, length);
    },
    async bytes() { return op_cache_read(key, path); },
    async text() { return new TextDecoder().decode(op_cache_read(key, path)); },
  });
}

const caches = Object.create(null);
for (const key of op_cache_keys()) {
  let cache;
  Object.defineProperty(caches, key, {
    enumerable: true,
    get() {
      return cache ??= globalThis.__polymerFilesystem.cacheResource(key, {
        open(path, options = {}) { return file(key, plainPath(path), options); },
      });
    },
  });
}
Object.freeze(caches);
Object.defineProperty(globalThis, "__polymerCaches", { value: caches });
