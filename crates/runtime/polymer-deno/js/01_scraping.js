import {
  op_scraping_package_from_path, op_scraping_jobs, op_scraping_start,
  op_scraping_job, op_scraping_retry, op_scraping_scrapers, op_scraping_refresh,
} from "ext:core/ops";

// Gumshoe-backed package values live in the UI realm as immutable branded objects.
export function createScraping() {
  const packageData = new WeakMap();
  const internal = Symbol("Polymer.scraping.Package");

  class Package {
    constructor(token, data) {
      if (token !== internal) throw new TypeError("Use Polymer.scraping.Package.fromPath()");
      packageData.set(this, Object.freeze({ ...data }));
      Object.freeze(this);
    }

    static async fromPath(path, options = undefined) {
      const signal = options?.signal;
      if (signal?.aborted) throw signal.reason ?? new DOMException("The operation was aborted", "AbortError");
      const pending = op_scraping_package_from_path(String(path));
      if (!signal) return new Package(internal, await pending);
      const aborted = new Promise((_, reject) => {
        signal.addEventListener("abort", () => reject(signal.reason ?? new DOMException("The operation was aborted", "AbortError")), { once: true });
      });
      return new Package(internal, await Promise.race([pending, aborted]));
    }

    get filename() { return packageData.get(this).filename; }
    get platform() { return packageData.get(this).platform; }
    get serial() { return packageData.get(this).serial; }
    get titleId() { return packageData.get(this).titleId; }
    get binaryDigest() { return packageData.get(this).binaryDigest; }
    toJSON() { return { ...packageData.get(this) }; }
  }

  const Scraping = new EventTarget();
  let dropActive = false;
  Object.defineProperties(Scraping, {
    Package: { value: Package },
    dropActive: { get: () => dropActive },
    jobs: { value: Object.freeze({
      list: () => op_scraping_jobs(),
      get: id => op_scraping_job(String(id)),
      start: (rootBinaryId, options = {}) => op_scraping_start(String(rootBinaryId), String(options.triggerBinaryId ?? rootBinaryId)),
      retry: id => op_scraping_retry(String(id)),
    }) },
    scrapers: { value: Object.freeze({
      list: () => op_scraping_scrapers(),
      update: id => op_scraping_refresh(String(id)),
    }) },
  });
  return Object.freeze({
    Scraping,
    serialize(value) {
      const data = packageData.get(value);
      if (!data) throw new TypeError("Expected a Polymer.scraping.Package");
      return data;
    },
    request(detail) {
      Scraping.dispatchEvent(new CustomEvent("request", { detail: Object.freeze(detail) }));
    },
    setDropActive(active) {
      active = Boolean(active);
      if (dropActive === active) return;
      dropActive = active;
      Scraping.dispatchEvent(new CustomEvent("dropactivechange", { detail: active }));
    },
    jobChanged(job) {
      Scraping.dispatchEvent(new CustomEvent("jobstatechange", { detail: Object.freeze(job) }));
    },
  });
}
