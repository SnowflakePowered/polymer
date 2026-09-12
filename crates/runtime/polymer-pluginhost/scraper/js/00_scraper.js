let generator;

async function initialize(entrypoint, input) {
  const namespace = await import(entrypoint);
  const callback = namespace.onScrape ?? namespace.default?.onScrape;
  if (typeof callback !== "function") {
    throw new TypeError("plugin export 'onScrape' is not a function");
  }
  generator = Reflect.apply(callback, namespace, [input]);
  if (Object.prototype.toString.call(generator) !== "[object AsyncGenerator]" ||
      generator[Symbol.asyncIterator]?.() !== generator) {
    throw new TypeError("onScrape must return an async generator");
  }
}

async function next() {
  const step = await generator.next();
  if (step.done) return { done: true };
  const value = step.value;
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError("onScrape must yield { key, value, source } objects");
  }
  // Validate byte values before serde_v8 converts them into host bytes.
  const media = value.value;
  if (media !== null && typeof media === "object") {
    if (media.kind !== "inline" || !Array.isArray(media.bytes)) {
      throw new TypeError("inline scrape media requires a byte array");
    }
    for (let index = 0; index < media.bytes.length; index++) {
      const byte = media.bytes[index];
      if (!Number.isInteger(byte) || byte < 0 || byte > 255) {
        throw new TypeError("inline media bytes must be integers between 0 and 255");
      }
    }
  }
  return { done: false, value };
}

Object.defineProperty(globalThis, "__polymerScraper", {
  value: Object.freeze({ initialize, next }),
  configurable: false,
  enumerable: false,
  writable: false,
});
