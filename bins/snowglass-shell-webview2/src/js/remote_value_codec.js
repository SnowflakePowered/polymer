// Shared live-reference value codec for the foreground and background bridge.
// Structured-clone values travel directly on the binary bus; this smaller
// envelope is only for functions and objects that must retain identity.
(() => {
  "use strict";

  function createRemoteValueCodec({ encodeReference, decodeReference }) {
    if (typeof encodeReference !== "function" || typeof decodeReference !== "function") {
      throw new TypeError("remote value codec requires reference adapters");
    }

    const encode = (value, seen = new Set()) => {
      if (value === undefined) return { kind: "undefined" };
      if (typeof value === "bigint") return { kind: "bigint", value: value.toString() };
      if (
        value === null ||
        typeof value === "string" ||
        typeof value === "number" ||
        typeof value === "boolean"
      ) {
        return { kind: "value", value };
      }
      if (typeof value === "function") return encodeReference(value, "function");
      if (typeof value !== "object") {
        throw new TypeError(`Cannot proxy a ${typeof value} value`);
      }
      if (seen.has(value)) return encodeReference(value, "cycle");

      const prototype = Object.getPrototypeOf(value);
      if (!Array.isArray(value) && prototype !== Object.prototype && prototype !== null) {
        return encodeReference(value, "nonPlain");
      }
      seen.add(value);
      try {
        if (Array.isArray(value)) {
          return { kind: "array", value: value.map((item) => encode(item, seen)) };
        }
        return {
          kind: "object",
          value: Object.fromEntries(
            Object.entries(value).map(([key, item]) => [key, encode(item, seen)]),
          ),
        };
      } finally {
        seen.delete(value);
      }
    };

    const decode = (wire) => {
      switch (wire?.kind) {
        case "undefined": return undefined;
        case "bigint": return BigInt(wire.value);
        case "array": return wire.value.map(decode);
        case "object": return Object.fromEntries(
          Object.entries(wire.value).map(([key, item]) => [key, decode(item)]),
        );
        case "value": return wire.value;
        default: return decodeReference(wire, decode);
      }
    };

    return Object.freeze({ encode, decode });
  }

  Object.defineProperty(globalThis, "__snowglassCreateRemoteValueCodec", {
    value: createRemoteValueCodec,
    configurable: false,
  });
})();
