interface Window {
  ipc: { postMessage(message: string): void };
  readonly gameLayer: EventTarget & {
    readonly contentWindow: BackgroundWindowProxy;
    postMessage(message: unknown): void;
  };
  __snowglassGameLayerMessage(message: unknown): void;
  __snowglassGameLayerProxyMessage(message: ProxyMessage): void;
  __snowglassSetInputState(state: unknown[], routed?: unknown[]): void;
  __snowglassSetInputPriority(priority: "game" | "foreground"): void;
  __snowglassKeyboardInput(event: unknown): void;
}

interface HTMLLinkElement {
  readonly contentWindow: BackgroundWindowProxy | null;
}

type BackgroundWindowProxy = {
  readonly [property: PropertyKey]: unknown;
};

type WireValue =
  | { kind: "value"; value: unknown }
  | { kind: "undefined" }
  | { kind: "bigint"; value: string }
  | { kind: "array"; value: WireValue[] }
  | { kind: "object"; value: Record<string, WireValue> }
  | { kind: "backgroundReference"; handle: number; path?: string[] }
  | { kind: "foregroundCallback"; handle: number };

type ProxyMessage =
  | {
      type: "response";
      id: number;
      ok: true;
      value: WireValue;
    }
  | {
      type: "response";
      id: number;
      ok: false;
      error: { name?: string; message?: string; stack?: string };
    }
  | {
      type: "callback";
      handle: number;
      args: WireValue[];
    };

let inputState: readonly unknown[] = Object.freeze([]);
let inputPriority: "game" | "foreground" = "foreground";

window.__snowglassSetInputState = (state, routed = []) => {
  inputState = Object.freeze([...state]);
  dispatchEvent(new CustomEvent("snowglassinputstate", { detail: inputState }));
  for (const event of routed) {
    dispatchEvent(new CustomEvent("snowglassinput", { detail: event }));
  }
};

window.__snowglassSetInputPriority = (priority) => {
  inputPriority = priority;
  dispatchEvent(new CustomEvent("snowglassinputpriority", { detail: priority }));
};

window.__snowglassKeyboardInput = (event) => {
  dispatchEvent(new CustomEvent("snowglassinput", { detail: event }));
};

Object.defineProperty(window, "snowglassInput", {
  value: Object.freeze({
    get controllers() {
      return inputState;
    },
    get priority() {
      return inputPriority;
    },
  }),
  writable: false,
});

const remoteReference = Symbol("snowglass.backgroundReference");
const pendingProxyRequests = new Map<
  number,
  { resolve(value: unknown): void; reject(error: Error): void }
>();
const foregroundCallbacks = new Map<number, (...args: unknown[]) => unknown>();
const foregroundCallbackIds = new WeakMap<(...args: never[]) => unknown, number>();
let nextProxyRequest = 1;
let nextForegroundCallback = 1;

function postToBackground(message: unknown): void {
  window.ipc.postMessage(JSON.stringify({ type: "gameLayerMessage", message }));
}

function encodeWireValue(value: unknown, seen = new Set<object>()): WireValue {
  if (value === undefined) return { kind: "undefined" };
  if (typeof value === "bigint") {
    return { kind: "bigint", value: value.toString() };
  }
  if (
    value === null ||
    typeof value === "string" ||
    typeof value === "number" ||
    typeof value === "boolean"
  ) {
    return { kind: "value", value };
  }

  const reference = (
    value as { [remoteReference]?: { handle: number; path: string[] } }
  )[remoteReference];
  if (reference) {
    return {
      kind: "backgroundReference",
      handle: reference.handle,
      path: reference.path,
    };
  }

  if (typeof value === "function") {
    let handle = foregroundCallbackIds.get(
      value as (...args: never[]) => unknown,
    );
    if (handle === undefined) {
      handle = nextForegroundCallback++;
      foregroundCallbackIds.set(value as (...args: never[]) => unknown, handle);
      foregroundCallbacks.set(handle, value as (...args: unknown[]) => unknown);
    }
    return { kind: "foregroundCallback", handle };
  }

  if (typeof value !== "object") {
    throw new TypeError(`Cannot send a ${typeof value} to the background`);
  }
  if (seen.has(value)) {
    throw new TypeError("Cannot send a cyclic object to the background");
  }
  seen.add(value);
  try {
    if (Array.isArray(value)) {
      return {
        kind: "array",
        value: value.map((item) => encodeWireValue(item, seen)),
      };
    }
    return {
      kind: "object",
      value: Object.fromEntries(
        Object.entries(value).map(([key, item]) => [
          key,
          encodeWireValue(item, seen),
        ]),
      ),
    };
  } finally {
    seen.delete(value);
  }
}

function decodeWireValue(value: WireValue): unknown {
  switch (value.kind) {
    case "undefined":
      return undefined;
    case "bigint":
      return BigInt(value.value);
    case "array":
      return value.value.map(decodeWireValue);
    case "object":
      return Object.fromEntries(
        Object.entries(value.value).map(([key, item]) => [
          key,
          decodeWireValue(item),
        ]),
      );
    case "backgroundReference":
      return createBackgroundProxy(value.handle, value.path || []);
    case "value":
      return value.value;
  }
}

function requestBackground(
  operation: "get" | "apply" | "set" | "delete",
  handle: number,
  path: string[],
  payload: Record<string, unknown> = {},
): Promise<unknown> {
  const id = nextProxyRequest++;
  const promise = new Promise<unknown>((resolve, reject) => {
    pendingProxyRequests.set(id, { resolve, reject });
  });
  window.ipc.postMessage(
    JSON.stringify({
      type: "gameLayerProxy",
      request: { id, operation, handle, path, ...payload },
    }),
  );
  return promise;
}

function reportProxyError(error: unknown): void {
  const event = new ErrorEvent("error", {
    cancelable: true,
    error,
    message: error instanceof Error ? error.message : String(error),
  });
  gameLayerBridge.dispatchEvent(event);
  if (!event.defaultPrevented) console.error(error);
}

function createBackgroundProxy(
  handle: number,
  path: string[] = [],
): BackgroundWindowProxy {
  const target = function backgroundRemoteObject() {};
  return new Proxy(target, {
    get(_target, property) {
      if (property === remoteReference) return { handle, path };
      if (property === Symbol.toStringTag) {
        return path.length === 0 && handle === 0
          ? "BackgroundWindowProxy"
          : "BackgroundObjectProxy";
      }
      if (property === "then") {
        if (path.length === 0) return undefined;
        return (resolve: (value: unknown) => void, reject: (error: Error) => void) => {
          requestBackground("get", handle, path).then(resolve, reject);
        };
      }
      if (handle === 0 && path.length === 0 && property === "postMessage") {
        return postToBackground;
      }
      if (typeof property !== "string") return undefined;
      return createBackgroundProxy(handle, [...path, property]);
    },
    apply(_target, _thisArgument, args) {
      return requestBackground("apply", handle, path, {
        args: args.map((value) => encodeWireValue(value)),
      });
    },
    set(_target, property, value) {
      if (typeof property !== "string") return false;
      void requestBackground("set", handle, [...path, property], {
        value: encodeWireValue(value),
      }).catch(reportProxyError);
      return true;
    },
    deleteProperty(_target, property) {
      if (typeof property !== "string") return false;
      void requestBackground("delete", handle, [...path, property]).catch(
        reportProxyError,
      );
      return true;
    },
  }) as BackgroundWindowProxy;
}

const backgroundWindowProxy = createBackgroundProxy(0);

// A Worker-shaped endpoint for ordinary messages, plus an asynchronous
// same-document proxy for accessing the Blitz/Deno background realm.
const gameLayerBridge = new EventTarget() as EventTarget & {
  readonly contentWindow: BackgroundWindowProxy;
  postMessage(message: unknown): void;
};
gameLayerBridge.postMessage = postToBackground;
Object.defineProperty(gameLayerBridge, "contentWindow", {
  value: backgroundWindowProxy,
  writable: false,
});
Object.defineProperty(window, "gameLayer", {
  value: gameLayerBridge,
  writable: false,
});
window.__snowglassGameLayerMessage = (message) => {
  gameLayerBridge.dispatchEvent(new MessageEvent("message", { data: message }));
};
window.__snowglassGameLayerProxyMessage = (message) => {
  if (message.type === "callback") {
    const callback = foregroundCallbacks.get(message.handle);
    if (!callback) return;
    try {
      callback(...message.args.map(decodeWireValue));
    } catch (error) {
      reportProxyError(error);
    }
    return;
  }
  const pending = pendingProxyRequests.get(message.id);
  if (!pending) return;
  pendingProxyRequests.delete(message.id);
  if (message.ok) {
    pending.resolve(decodeWireValue(message.value));
  } else {
    const error = new Error(message.error.message || "Background proxy failed");
    error.name = message.error.name || "Error";
    if (message.error.stack) error.stack = message.error.stack;
    pending.reject(error);
  }
};

Object.defineProperty(HTMLLinkElement.prototype, "contentWindow", {
  get() {
    return this.relList.contains("background") ? backgroundWindowProxy : null;
  },
  configurable: true,
});

let frontendReadyAnnounced = false;

async function announceFrontendReady(): Promise<void> {
  if (
    frontendReadyAnnounced ||
    window.top !== window ||
    location.href === "about:blank"
  ) {
    return;
  }
  frontendReadyAnnounced = true;

  const message: {
    type: "frontendReady";
    background?: { url: string; html: string };
    backgroundError?: string;
  } = { type: "frontendReady" };
  const link = document.querySelector<HTMLLinkElement>(
    'link[rel~="background"][href]',
  );
  if (link) {
    try {
      const response = await fetch(link.href);
      if (!response.ok) {
        throw new Error(`${response.status} ${response.statusText}`.trim());
      }
      message.background = {
        url: response.url || link.href,
        html: await response.text(),
      };
    } catch (error) {
      message.backgroundError = `${link.href}: ${String(error)}`;
    }
  }
  window.ipc.postMessage(JSON.stringify(message));
}

if (document.readyState === "loading") {
  addEventListener("DOMContentLoaded", () => void announceFrontendReady(), {
    once: true,
  });
} else {
  queueMicrotask(() => void announceFrontendReady());
}
