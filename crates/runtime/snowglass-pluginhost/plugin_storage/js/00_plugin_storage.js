import {
  op_plugin_storage_get,
  op_plugin_storage_set,
} from "ext:core/ops";

const pluginStorage = Object.freeze({
  getItem(key) {
    return op_plugin_storage_get(key) ?? null;
  },
  setItem(key, value) {
    op_plugin_storage_set(key, value);
  },
});

Object.defineProperty(globalThis, "__snowglassPluginStorage", {
  value: pluginStorage,
  configurable: false,
  enumerable: false,
  writable: false,
});
