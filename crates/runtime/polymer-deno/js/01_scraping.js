import {
  op_scraping_package_from_path, op_scraping_start, op_scraping_scrapers,
} from "ext:core/ops";

import { Package as PackageValue } from "./00_package.js";

// Gumshoe-backed package values live in the UI realm as immutable branded objects.
export function createScraping() {
  const internal = Symbol("Polymer.scraping.Package");

  class Package extends PackageValue {
    constructor(token, data) {
      if (token !== internal) throw new TypeError("Use Polymer.scraping.Package.fromPath()");
      super(data);
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
  }

  const Scraping = new EventTarget();
  let dropActive = false;
  Object.defineProperties(Scraping, {
    Package: { value: Package },
    dropActive: { get: () => dropActive },
    start: { value: (rootBinaryId, options = {}) => op_scraping_start(String(rootBinaryId), String(options.triggerBinaryId ?? rootBinaryId)) },
    scrapers: { value: Object.freeze({
      list: () => op_scraping_scrapers(),
    }) },
  });
  return Object.freeze({
    Scraping,
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
}
