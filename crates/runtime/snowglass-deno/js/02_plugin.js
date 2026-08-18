import { op_plugin_invoke, op_plugin_list } from "ext:core/ops";

// Same-realm plugin facade. Calls cross only the Deno op boundary; pluginhost
// keeps each package invocation in its own fresh worker isolate.
export function createPlugin() {
  const pluginId = (id) => {
    if (typeof id !== "string" || !/^[A-Za-z0-9_-]+$/.test(id)) {
      throw new TypeError("plugin ID must contain only letters, digits, '-' or '_'");
    }
    return id;
  };

  const descriptor = (value) => Object.freeze({
    id: String(value.id),
    version: String(value.version),
    class: String(value.class),
    caches: Object.freeze((value.caches ?? []).map((cache) => Object.freeze({
      key: String(cache.key),
      label: String(cache.label),
    }))),
  });

  const list = () => Object.freeze(op_plugin_list().map(descriptor));

  const errorFrom = (wire) => {
    const error = new Error(String(wire?.message ?? "plugin invocation failed"));
    error.name = String(wire?.name ?? "Error");
    if (wire?.stack) error.stack = String(wire.stack);
    Object.defineProperty(error, "code", {
      value: String(wire?.code ?? "PLUGIN_WORKER_FAILED"),
      enumerable: true,
    });
    return error;
  };

  const assertJson = (value, ancestors = new Set()) => {
    if (value === null || typeof value === "string" || typeof value === "boolean") return;
    if (typeof value === "number" && Number.isFinite(value)) return;
    if (typeof value !== "object") {
      throw new TypeError("plugin arguments must contain only JSON-compatible values");
    }
    if (ancestors.has(value)) throw new TypeError("plugin arguments must not contain cycles");
    if (!Array.isArray(value) && Object.getPrototypeOf(value) !== Object.prototype && Object.getPrototypeOf(value) !== null) {
      throw new TypeError("plugin arguments must contain only arrays and plain objects");
    }
    ancestors.add(value);
    if (Array.isArray(value)) {
      for (const item of value) assertJson(item, ancestors);
    } else {
      for (const item of Object.values(value)) assertJson(item, ancestors);
    }
    ancestors.delete(value);
  };

  const invoke = async (plugin, path, args) => {
    assertJson(args);
    const result = await op_plugin_invoke(plugin, path, args);
    if (!result?.ok) throw errorFrom(result?.error);
    return result.value;
  };

  const callable = (plugin, path = []) => new Proxy(function () {}, {
    get(_target, property) {
      if (property === "then" && path.length === 0) return undefined;
      if (property === Symbol.toStringTag) return "SnowglassPluginExport";
      if (typeof property !== "string") return undefined;
      return callable(plugin, [...path, property]);
    },
    apply(_target, _receiver, args) {
      if (path.length === 0) {
        return Promise.reject(errorFrom({
          name: "TypeError",
          code: "INVALID_EXPORT_PATH",
          message: "a plugin export property must be selected before calling",
        }));
      }
      return invoke(plugin, path, args);
    },
  });

  return Object.freeze({
    list,
    get(id) {
      id = pluginId(id);
      return list().find((plugin) => plugin.id === id);
    },
    open(id) {
      return callable(pluginId(id));
    },
  });
}
