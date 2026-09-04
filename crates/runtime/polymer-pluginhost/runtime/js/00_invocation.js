async function invoke(entrypoint, path, args) {
  const namespace = await import(entrypoint);
  let receiver;
  let value;
  if (path[0] === "default") {
    value = namespace.default;
    receiver = namespace;
    for (const segment of path.slice(1)) {
      receiver = value;
      value = value?.[segment];
    }
  } else {
    value = namespace[path[0]];
    receiver = namespace;
    if (value === undefined) {
      receiver = namespace.default;
      value = receiver?.[path[0]];
    }
    for (const segment of path.slice(1)) {
      receiver = value;
      value = value?.[segment];
    }
  }
  if (typeof value !== "function") {
    throw new TypeError(`plugin export '${path.join(".")}' is not a function`);
  }
  return await Reflect.apply(value, receiver, args);
}

async function invokeOptional(entrypoint, name, args) {
  const namespace = await import(entrypoint);
  let receiver = namespace;
  let value = namespace[name];
  if (value === undefined) {
    receiver = namespace.default;
    value = receiver?.[name];
  }
  if (value === undefined) return { found: false, value: null };
  if (typeof value !== "function") {
    throw new TypeError(`plugin export '${name}' is not a function`);
  }
  const result = await Reflect.apply(value, receiver, args);
  return { found: true, value: result === undefined ? null : result };
}

Object.defineProperty(globalThis, "__polymerPluginRuntime", {
  value: Object.freeze({ invoke, invokeOptional }),
  configurable: false,
  enumerable: false,
  writable: false,
});
