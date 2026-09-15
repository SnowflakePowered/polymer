import { createPackage } from "ext:polymer_digest/00_digest.js";
import { op_plugin_emit_event } from "ext:core/ops";
const progressContext = Object.freeze({ progress: ({ message, completed = null, total = null }) => {
    if (typeof message !== "string" ||
        (completed !== null && (!Number.isFinite(completed) || completed < 0)) ||
        (total !== null && (!Number.isFinite(total) || total <= 0)) ||
        (completed !== null && total !== null && completed > total)) {
      throw new TypeError("invalid plugin progress");
    }
    op_plugin_emit_event("loadprogress", { state: "loading", message, completed, total });
  } });
async function invoke(entrypoint, path, args) {
  args = args.map(argument => {
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
  if (path.join(".") === "updateContents" || path.join(".") === "default.updateContents") args = [progressContext];
  if (path.at(-1) === "onBootstrap") {
    const result = await globalThis.__polymerPlatform.bootstrap(receiver, args[0], Object.freeze({ ...args[1], ...progressContext }));
    return result === undefined ? null : result;
  }
  const result = await Reflect.apply(value, receiver, args);
  return result === undefined ? null : result;
}

async function invokeOptional(entrypoint, name, args) {
  args = args.map(argument => {
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
  if (name === "onPluginLoad") args = [progressContext];
  const result = await Reflect.apply(value, receiver, args);
  return { found: true, value: result === undefined ? null : result };
}

Object.defineProperty(globalThis, "__polymerPluginRuntime", {
  value: Object.freeze({ invoke, invokeOptional }),
  configurable: false,
  enumerable: false,
  writable: false,
});

