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
  return { done: false, value };
}

Object.defineProperty(globalThis, "__polymerScraper", {
  value: Object.freeze({ initialize, next }),
  configurable: false,
  enumerable: false,
  writable: false,
});
