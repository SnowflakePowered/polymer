// Common lazy Promise-based facade installed in both Snowglass DOM realms.
// The realm adapter supplies __snowglassPluginTransport.invoke(). No package
// module is resolved until a proxy property chain is actually called.
(() => {
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

  const invoke = (plugin, path, args) => Promise.resolve().then(async () => {
    const transport = globalThis.__snowglassPluginTransport;
    if (!transport || typeof transport.invoke !== "function") {
      throw errorFrom({
        name: "InvalidStateError",
        code: "PLUGIN_TRANSPORT_UNAVAILABLE",
        message: "Snowglass plugin transport is unavailable",
      });
    }
    try {
      return await transport.invoke(plugin, path, args);
    } catch (error) {
      if (!(error instanceof Error)) throw errorFrom(error);
      if (!("code" in error)) {
        Object.defineProperty(error, "code", {
          value: "PLUGIN_WORKER_FAILED",
          enumerable: true,
        });
      }
      throw error;
    }
  });

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

  const current = globalThis.snowglass && typeof globalThis.snowglass === "object"
    ? globalThis.snowglass
    : {};
  Object.defineProperty(current, "plugins", {
    value: Object.freeze({
      open(id) {
        if (typeof id !== "string" || !/^[A-Za-z0-9_-]+$/.test(id)) {
          throw new TypeError("plugin ID must contain only letters, digits, '-' or '_'");
        }
        return callable(id);
      },
    }),
    enumerable: true,
    configurable: false,
  });
  Object.defineProperty(globalThis, "snowglass", {
    value: current,
    configurable: false,
  });

})();
