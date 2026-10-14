import { op_plugin_context } from "ext:core/ops";

function frozen(value) {
  if (value !== null && typeof value === "object") {
    for (const child of Object.values(value)) frozen(child);
    Object.freeze(value);
  }
  return value;
}

Object.defineProperty(globalThis, "__polymerPlugin", {
  value: frozen(op_plugin_context()),
  configurable: false,
  enumerable: false,
  writable: false,
});
