interface Window {
  ipc: { postMessage(message: string): void };
  readonly gameLayer: EventTarget & {
    readonly contentWindow: BackgroundWindowProxy;
    readonly ready: boolean;
    postMessage(message: unknown): void;
  };
  readonly Emulator: BackgroundWindowProxy;
  __snowglassGameLayerMessage(message: unknown): void;
  __snowglassGameLayerProxyMessage(message: ProxyMessage): void;
  __snowglassGameLayerReady(): void;
  __snowglassSetInputState(state: unknown[], routed?: unknown[]): void;
  __snowglassSetInputPriority(priority: "game" | "foreground"): void;
  __snowglassKeyboardInput(event: unknown): void;
  __snowglassSetInputConfiguration(
    configuration: Record<string, unknown>,
    layouts: unknown[],
  ): void;
  __snowglassInputConfigurationResult(
    id: number,
    result: {
      ok: boolean;
      configuration?: Record<string, unknown>;
      error?: string;
    },
  ): void;
}

type ForegroundHitRegion = {
  x: number;
  y: number;
  width: number;
  height: number;
};

interface HTMLLinkElement {
  readonly contentWindow: BackgroundWindowProxy | null;
}

type BackgroundWindowProxy = {
  (...args: unknown[]): Promise<unknown>;
  readonly [property: string]: BackgroundWindowProxy;
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
let inputTimestamp = 0;
let inputConfiguration: Readonly<Record<string, unknown>> = Object.freeze({
  revision: 0,
  ports: Object.freeze([]),
});
let inputLayouts: readonly unknown[] = Object.freeze([]);
let nextInputRequest = 1;
const snowglassInputTarget = new EventTarget();
const pendingInputRequests = new Map<
  number,
  {
    resolve(value: Readonly<Record<string, unknown>>): void;
    reject(error: Error): void;
  }
>();

window.__snowglassSetInputState = (state, routed = []) => {
  inputState = Object.freeze([...state]);
  inputTimestamp = performance.now();
  dispatchEvent(new CustomEvent("snowglassinputstate", { detail: inputState }));
  snowglassInputTarget.dispatchEvent(
    new CustomEvent("inputstatechange", { detail: inputState }),
  );
  for (const event of routed) {
    dispatchEvent(new CustomEvent("snowglassinput", { detail: event }));
  }
};

function webGamepads(): readonly unknown[] {
  const devices = inputState.filter(
    (device): device is {
      index: number;
      deviceId: string;
      name: string;
      guid: string;
      vendor?: number;
      product?: number;
      assignedPorts: number[];
      buttons: { pressed: boolean; mapped: boolean }[];
      axes: { value: number; mapped: boolean }[];
    } => typeof device === "object" && device !== null,
  );
  const result = Array.from<unknown>({
    length: devices.reduce((length, device) => Math.max(length, device.index + 1), 0),
  }).fill(null);
  for (const device of devices) {
    const visible = (mapped: boolean) =>
      inputPriority === "foreground" || !mapped;
    result[device.index] = Object.freeze({
      index: device.index,
      deviceId: device.deviceId,
      id: `${device.name} (${device.guid})`,
      connected: true,
      mapping: "standard",
      timestamp: inputTimestamp,
      assignedPorts: Object.freeze([...device.assignedPorts]),
      buttons: Object.freeze(
        device.buttons.map((button) => {
          const pressed = visible(button.mapped) && button.pressed;
          return Object.freeze({
            pressed,
            touched: pressed,
            value: pressed ? 1 : 0,
          });
        }),
      ),
      axes: Object.freeze(
        device.axes.map((axis) =>
          visible(axis.mapped)
            ? Math.max(-1, Math.min(1, axis.value / 32767))
            : 0,
        ),
      ),
      metadata: Object.freeze({
        guid: device.guid,
        vendor: device.vendor,
        product: device.product,
      }),
    });
  }
  return Object.freeze(result);
}

window.__snowglassSetInputPriority = (priority) => {
  inputPriority = priority;
  dispatchEvent(new CustomEvent("snowglassinputpriority", { detail: priority }));
  snowglassInputTarget.dispatchEvent(
    new CustomEvent("inputlayerchange", { detail: priority }),
  );
};

window.__snowglassKeyboardInput = (event) => {
  dispatchEvent(new CustomEvent("snowglassinput", { detail: event }));
};

window.__snowglassSetInputConfiguration = (configuration, layouts) => {
  inputConfiguration = Object.freeze({ ...configuration });
  inputLayouts = Object.freeze([...layouts]);
  dispatchEvent(
    new CustomEvent("snowglassinputconfigurationchange", {
      detail: inputConfiguration,
    }),
  );
  snowglassInputTarget.dispatchEvent(
    new CustomEvent("configurationchange", { detail: inputConfiguration }),
  );
};

window.__snowglassInputConfigurationResult = (id, result) => {
  const pending = pendingInputRequests.get(id);
  if (!pending) return;
  pendingInputRequests.delete(id);
  if (!result.ok || !result.configuration) {
    pending.reject(new Error(result.error || "Input configuration failed"));
    return;
  }
  inputConfiguration = Object.freeze({ ...result.configuration });
  pending.resolve(inputConfiguration);
};

let publishedForegroundHitRegions = "";
let foregroundHitRegionFrame = 0;
const foregroundHitRegionElements = new Set<Element>();
const foregroundHitRegionResizeObserver = new ResizeObserver(() => {
  scheduleForegroundHitRegions();
});

function collectForegroundHitRegions(): ForegroundHitRegion[] {
  const scale = window.devicePixelRatio;
  const regions: ForegroundHitRegion[] = [];
  for (const element of document.querySelectorAll(
    "[data-snowglass-foreground-hit-region]",
  )) {
    const style = getComputedStyle(element);
    if (
      style.display === "none" ||
      style.visibility === "hidden" ||
      style.pointerEvents === "none"
    ) {
      continue;
    }
    for (const rect of element.getClientRects()) {
      const left = Math.floor(rect.left * scale);
      const top = Math.floor(rect.top * scale);
      const right = Math.ceil(rect.right * scale);
      const bottom = Math.ceil(rect.bottom * scale);
      if (right > left && bottom > top) {
        regions.push({
          x: left,
          y: top,
          width: right - left,
          height: bottom - top,
        });
      }
    }
  }
  return regions;
}

function publishForegroundHitRegions(): void {
  foregroundHitRegionFrame = 0;
  const regions = collectForegroundHitRegions();
  const serialized = JSON.stringify(regions);
  if (serialized !== publishedForegroundHitRegions) {
    publishedForegroundHitRegions = serialized;
    window.ipc.postMessage(
      JSON.stringify({ type: "foregroundHitRegions", regions }),
    );
  }

  const current = new Set(
    document.querySelectorAll("[data-snowglass-foreground-hit-region]"),
  );
  for (const element of foregroundHitRegionElements) {
    if (!current.has(element)) {
      foregroundHitRegionResizeObserver.unobserve(element);
      foregroundHitRegionElements.delete(element);
    }
  }
  for (const element of current) {
    if (!foregroundHitRegionElements.has(element)) {
      foregroundHitRegionElements.add(element);
      foregroundHitRegionResizeObserver.observe(element);
    }
  }
}

function scheduleForegroundHitRegions(): void {
  if (!foregroundHitRegionFrame) {
    foregroundHitRegionFrame = requestAnimationFrame(publishForegroundHitRegions);
  }
}

new MutationObserver(scheduleForegroundHitRegions).observe(document, {
  attributes: true,
  childList: true,
  subtree: true,
});
addEventListener("resize", scheduleForegroundHitRegions);
addEventListener("transitionrun", scheduleForegroundHitRegions, true);
addEventListener("transitionend", scheduleForegroundHitRegions, true);

Object.defineProperties(snowglassInputTarget, {
  controllers: {
    get: () => inputState,
  },
  getGamepads: {
    value: () => webGamepads(),
  },
  priority: {
    get: () => inputPriority,
  },
  configuration: {
    get: () => inputConfiguration,
  },
  revision: {
    get: () => Number(inputConfiguration.revision) || 0,
  },
  ports: {
    get: () =>
      (inputConfiguration.ports as readonly unknown[]) || Object.freeze([]),
  },
  layouts: {
    get: () => inputLayouts,
  },
  getControllerLayout: {
    value: (id: string) =>
      inputLayouts.find(
        (layout) =>
          typeof layout === "object" &&
          layout !== null &&
          (layout as { id?: unknown }).id === id,
      ) || null,
  },
  configure: {
    value: (transaction: Record<string, unknown>) => {
      const id = nextInputRequest++;
      const wire = {
        ...transaction,
        expectedRevision:
          transaction.expectedRevision ?? inputConfiguration.revision ?? 0,
      };
      return new Promise<Readonly<Record<string, unknown>>>((resolve, reject) => {
        pendingInputRequests.set(id, { resolve, reject });
        window.ipc.postMessage(
          JSON.stringify({ type: "inputConfigure", id, transaction: wire }),
        );
      });
    },
  },
});
Object.freeze(snowglassInputTarget);
Object.defineProperty(window, "snowglassInput", {
  value: snowglassInputTarget,
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
  readonly ready: boolean;
  postMessage(message: unknown): void;
};
let gameLayerReady = false;
gameLayerBridge.postMessage = postToBackground;
Object.defineProperty(gameLayerBridge, "contentWindow", {
  value: backgroundWindowProxy,
  writable: false,
});
Object.defineProperty(gameLayerBridge, "ready", {
  get: () => gameLayerReady,
});
Object.defineProperty(window, "gameLayer", {
  value: gameLayerBridge,
  writable: false,
});
Object.defineProperty(window, "Emulator", {
  value: backgroundWindowProxy.Emulator,
  writable: false,
});
window.__snowglassGameLayerMessage = (message) => {
  gameLayerBridge.dispatchEvent(new MessageEvent("message", { data: message }));
};
window.__snowglassGameLayerReady = () => {
  if (gameLayerReady) return;
  gameLayerReady = true;
  gameLayerBridge.dispatchEvent(new Event("load"));
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
  addEventListener(
    "DOMContentLoaded",
    () => {
      scheduleForegroundHitRegions();
      void announceFrontendReady();
    },
    { once: true },
  );
} else {
  scheduleForegroundHitRegions();
  queueMicrotask(() => void announceFrontendReady());
}
