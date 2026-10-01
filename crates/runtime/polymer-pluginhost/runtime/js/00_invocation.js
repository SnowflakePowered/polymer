import { createPackage } from "ext:polymer_digest/00_digest.js";
import { hosted } from "ext:polymer_plugin_runtime/01_jobs.js";
function bindArguments(args) {
  return args.map(argument => {
    if (argument?.__digestToken !== undefined) return createPackage(argument);
    if (argument?.package === undefined && argument?.launchSet === undefined) return argument;
    return {
      ...argument,
      ...(argument.package === undefined ? {} : { package: createPackage(argument.package) }),
      ...(argument.launchSet === undefined ? {} : {
        launchSet: argument.launchSet.map(disc => ({ ...disc, package: createPackage(disc.package) })),
      }),
    };
  });
}

async function invoke(entrypoint, path, args, job) {
  args = bindArguments(args);
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
  if (path.join(".") === "updateContents" || path.join(".") === "default.updateContents") args = [];
  const platform = { onBootstrap: "bootstrap", onCheckUpdates: "checkUpdates", onFetchUpdate: "fetchUpdate" }[path.at(-1)];
  if (platform) {
    const result = await hosted(job, () => globalThis.__polymerPlatform[platform](receiver, ...args));
    return result === undefined ? null : result;
  }
  const result = await hosted(job, () => Reflect.apply(value, receiver, args));
  return result === undefined ? null : result;
}

async function invokeOptional(entrypoint, name, args) {
  args = bindArguments(args);
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
  if (name === "onPluginLoad") args = [];
  const result = await Reflect.apply(value, receiver, args);
  return { found: true, value: result === undefined ? null : result };
}

async function exports(entrypoint) {
  const namespace = await import(entrypoint);
  const named = Object.entries(namespace);
  const fallback = namespace.default !== null && typeof namespace.default === "object"
    ? Object.entries(namespace.default)
    : [];
  return [...new Set([...named, ...fallback].filter(([, value]) => typeof value === "function").map(([name]) => name))];
}

Object.defineProperty(globalThis, "__polymerPluginRuntime", {
  value: Object.freeze({ invoke, invokeOptional, exports }),
  configurable: false,
  enumerable: false,
  writable: false,
});

