import { Package } from "./00_package.js";
import {
  op_contents_refresh, op_plugin_cache_clear, op_plugin_cache_size, op_plugin_invoke, op_plugin_list,
  op_plugin_options, op_plugin_set_option,
} from "ext:core/ops";

// Same-realm plugin facade. Calls cross only the Deno op boundary; pluginhost
// keeps each package invocation in its own fresh worker isolate.
export function createPlugin() {
  const pluginId = (id) => {
    if (typeof id !== "string" || !/^[A-Za-z0-9_-]+$/.test(id)) {
      throw new TypeError("plugin ID must contain only letters, digits, '-' or '_'");
    }
    return id;
  };

  const errorFrom = (wire) => {
    const error = new Error(String(wire?.message ?? "plugin invocation failed"));
    error.name = String(wire?.name ?? "Error");
    if (wire?.details) Object.defineProperty(error, "details", { value: wire.details, enumerable: true });
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
    if (value instanceof Package) return;
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
    const result = await op_plugin_invoke(plugin, path, Package.serialize(args));
    if (!result?.ok) throw errorFrom(result?.error);
    return result.value;
  };

  const callable = (plugin, path = []) => new Proxy(function () {}, {
    get(_target, property) {
      if (property === "then" && path.length === 0) return undefined;
      if (property === Symbol.toStringTag) return "PolymerPluginExport";
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

  const plugin = (value) => {
    const id = String(value.id);
    const options = (value.config ?? []).map((option) => ({
      key: String(option.key),
      display: String(option.display),
      default: option.default,
      choices: Object.freeze([...option.choices]),
    }));
    return Object.freeze({
      id,
      version: String(value.version),
      class: String(value.class),
      caches: Object.freeze(Object.fromEntries((value.caches ?? []).map((cache) => {
        const key = String(cache.key);
        return [key, Object.freeze({
          key,
          label: String(cache.label),
          size: () => op_plugin_cache_size(id, key),
          clear: () => op_plugin_cache_clear(id, key),
        })];
      }))),
      async config() {
        const values = await op_plugin_options(id);
        return Object.freeze(options.map((option) => Object.freeze({ ...option, value: values[option.key] })));
      },
      exports: callable(id),
      updateContents: () => op_contents_refresh(id),
      configure: (key, value) => op_plugin_set_option(id, String(key), value),
    });
  };

  const list = () => Object.freeze(op_plugin_list().map(plugin));

  const target = new EventTarget();
  const latest = new Map();
  const api = Object.assign(target, {
    events: () => Object.freeze([...latest.values()]),
    list,
    get(id) {
      id = pluginId(id);
      return list().find((plugin) => plugin.id === id);
    },
  });
  return Object.freeze({
    Plugin: api,
    dispatch(event) {
      const value = Object.freeze({ ...event });
      const key = `${value.sourceId}:${value.name}`;
      latest.delete(key);
      latest.set(key, value);
      if (latest.size > 256) latest.delete(latest.keys().next().value);
      target.dispatchEvent(new CustomEvent(value.name, { detail: value }));
    },
  });
}
