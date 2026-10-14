import { op_unarchive } from "ext:core/ops";
import { cacheFileLocation } from "ext:polymer_cache/00_cache.js";
import { ambientJob } from "ext:polymer_plugin_runtime/01_jobs.js";

function unarchive(file, directory, options = {}) {
  const archive = cacheFileLocation(file);
  const destination = cacheFileLocation(directory);
  return ambientJob().then(parent =>
    op_unarchive(archive.key, archive.path, destination.key, destination.path, options, parent));
}

Object.defineProperty(globalThis, "__polymerUnarchive", { value: Object.freeze({ unarchive }) });
