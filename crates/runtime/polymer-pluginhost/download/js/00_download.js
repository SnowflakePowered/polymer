import { op_download } from "ext:core/ops";
import { cacheFileLocation } from "ext:polymer_cache/00_cache.js";
import { ambientJob } from "ext:polymer_plugin_runtime/01_jobs.js";

function download(url, file, options = {}) {
  const { key, path } = cacheFileLocation(file);
  return ambientJob().then(parent => op_download(String(url), path, key, options, parent));
}

Object.defineProperty(globalThis, "__polymerDownload", { value: Object.freeze({ download }) });
