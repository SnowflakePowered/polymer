// Gumshoe-backed package values live in the UI realm as immutable branded objects.
globalThis.__snowglassCreateScrapingFacade = (ops) => {
  const packageData = new WeakMap();
  const internal = Symbol("Snowglass.scraping.Package");

  class Package {
    constructor(token, data) {
      if (token !== internal) throw new TypeError("Use Snowglass.scraping.Package.fromPath()");
      packageData.set(this, Object.freeze({ ...data }));
      Object.freeze(this);
    }

    static async fromPath(path, options = undefined) {
      const signal = options?.signal;
      if (signal?.aborted) throw signal.reason ?? new DOMException("The operation was aborted", "AbortError");
      const pending = ops.op_scraping_package_from_path(String(path));
      if (!signal) return new Package(internal, await pending);
      const aborted = new Promise((_, reject) => {
        signal.addEventListener("abort", () => reject(signal.reason ?? new DOMException("The operation was aborted", "AbortError")), { once: true });
      });
      return new Package(internal, await Promise.race([pending, aborted]));
    }

    get path() { return packageData.get(this).path; }
    get name() { return packageData.get(this).name; }
    get platform() { return packageData.get(this).platform; }
    toJSON() { return { ...packageData.get(this) }; }
  }

  const Scraping = new EventTarget();
  let dropActive = false;
  Object.defineProperties(Scraping, {
    Package: { value: Package },
    dropActive: { get: () => dropActive },
  });
  return Object.freeze({
    Scraping,
    serialize(value) {
      const data = packageData.get(value);
      if (!data) throw new TypeError("Expected a Snowglass.scraping.Package");
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
  });
};
