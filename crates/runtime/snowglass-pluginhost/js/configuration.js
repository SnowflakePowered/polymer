import {
  op_configuration_entries,
  op_configuration_get,
  op_configuration_render,
  op_configuration_set,
} from "ext:core/ops";
import "./controller.js";

function configuration(id) {
  const proxies = new Map();

  function options(path = "") {
    let proxy = proxies.get(path);
    if (proxy !== undefined) {
      return proxy;
    }

    proxy = new Proxy(Object.create(null), {
      get(_target, property) {
        if (typeof property !== "string") {
          return undefined;
        }
        const child = path === "" ? property : `${path}.${property}`;
        const lookup = op_configuration_get(id, child);
        return lookup.found ? lookup.value : options(child);
      },
      set(_target, property, value) {
        if (typeof property !== "string") {
          return false;
        }
        const child = path === "" ? property : `${path}.${property}`;
        op_configuration_set(id, child, value);
        return true;
      },
    });
    proxies.set(path, proxy);
    return proxy;
  }

  return Object.freeze({
    options,
    entries() {
      return op_configuration_entries(id);
    },
    render() {
      const rendered = op_configuration_render(id);
      return Object.freeze(rendered);
    },
  });
}

Object.defineProperty(globalThis, "__snowglassPluginHost", {
  value: Object.freeze({ configuration }),
  configurable: false,
  enumerable: false,
  writable: false,
});
