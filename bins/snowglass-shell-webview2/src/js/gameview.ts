type GraphicsHook = "opengl" | "vulkan" | "d3d11" | "d3d12";
type ScalingMode = "integer" | "fit";

type TrackedSurface = {
  index: number;
  rotation: number;
  selector?: string;
  attach: HTMLElement;
  webrtc: boolean;
};

type GamePadMapping = {
  gamepadId: number | "K" | string;
  mouse: boolean;
  slot: number;
  layout: string;
  mapping: Record<string, string>;
};

type LaunchOptions = {
  command: string;
  arguments: string[];
  isolated: boolean;
  hook: GraphicsHook[];
  surfaces: TrackedSurface[];
  gamepadMapping: GamePadMapping[];
  webrtcBind?: string;
};

interface GameViewInstance {
  on(eventName: string, callback: (event: unknown) => void): void;
  readonly surfaces?: TrackedSurface[];
  message(messageName: string, args: unknown): void;
  close(): void;
}

interface GameViewManager {
  launch(options: LaunchOptions): GameViewInstance | null;
  readonly activeInstance?: GameViewInstance;
  activateInput(): void;
  deactivateInput(): void;
}

type NativeSurface = {
  connection: string;
  index: number;
  width: number;
  height: number;
};

type WireSurface = Omit<TrackedSurface, "attach"> & { attach: string };
type WireLaunchOptions = Omit<LaunchOptions, "surfaces"> & { surfaces: WireSurface[] };
type NativeEvent = {
  instanceId: string;
  eventName: string;
  payload: unknown;
};

type HostObject = {
  launch(options: string): string;
  close(instanceId: string): void;
  message(request: string): void;
  activateInput(): void;
  deactivateInput(): void;
};

interface Window {
  chrome: {
    webview: {
      hostObjects: { sync: { snowglass: HostObject } };
    };
  };
  ipc: { postMessage(message: string): void };
  gameView: GameViewManager;
  snowglassGameView: GameViewManager;
  readonly snowglassCliLaunchOptions?: WireLaunchOptions;
  __snowglassGameViewNativeEvent(event: NativeEvent): void;
  __snowglassSetInputState(state: unknown[], routed?: unknown[]): void;
  __snowglassSetInputPriority(priority: "game" | "foreground"): void;
  __snowglassKeyboardInput(event: unknown): void;
}

class GameViewTargetElement extends HTMLElement {
  #preferredSize = Object.freeze({ width: 0, height: 0 });

  static get observedAttributes(): string[] {
    return ["class", "scaling"];
  }

  attributeChangedCallback(): void {
    scheduleLayout();
  }

  connectedCallback(): void {
    scheduleLayout();
  }

  disconnectedCallback(): void {
    scheduleLayout();
  }

  get preferredSize(): Readonly<{ width: number; height: number }> {
    return this.#preferredSize;
  }

  get scaling(): ScalingMode {
    const attribute = this.getAttribute("scaling");
    if (attribute === "fit" || attribute === "integer") return attribute;
    return getComputedStyle(this).getPropertyValue("--game-view-scaling").trim() === "fit"
      ? "fit"
      : "integer";
  }

  set scaling(value: ScalingMode) {
    this.setAttribute("scaling", value === "fit" ? "fit" : "integer");
  }

  get integerScaling(): boolean {
    return this.scaling === "integer";
  }

  set integerScaling(enabled: boolean) {
    this.scaling = enabled ? "integer" : "fit";
  }

  setPreferredSize(value: { width?: number; height?: number }): void {
    const width = Math.max(0, Number(value?.width) || 0);
    const height = Math.max(0, Number(value?.height) || 0);
    if (this.#preferredSize.width === width && this.#preferredSize.height === height) return;
    this.#preferredSize = Object.freeze({ width, height });
    this.style.setProperty("--preferred-width", `${width}px`);
    this.style.setProperty("--preferred-height", `${height}px`);
    this.style.setProperty(
      "--preferred-aspect-ratio",
      width && height ? `${width} / ${height}` : "auto",
    );
    scheduleLayout();
  }
}

if (!customElements.get("game-view-target")) {
  customElements.define("game-view-target", GameViewTargetElement);
}

let inputState: readonly unknown[] = Object.freeze([]);
let inputPriority: "game" | "foreground" = "foreground";

const host = (): HostObject => window.chrome.webview.hostObjects.sync.snowglass;
const attachmentIds = new WeakMap<HTMLElement, string>();
let nextAttachmentId = 1;

function attachmentId(element: HTMLElement): string {
  let id = attachmentIds.get(element);
  if (!id) {
    id = element.dataset.gameViewAttachmentId || `game-view-attachment-${nextAttachmentId++}`;
    element.dataset.gameViewAttachmentId = id;
    attachmentIds.set(element, id);
  }
  return id;
}

function validateOptions(options: LaunchOptions): void {
  if (!options || typeof options.command !== "string" || !options.command.trim()) {
    throw new TypeError("GameView launch requires a command");
  }
  if (!Array.isArray(options.surfaces) || options.surfaces.length === 0) {
    throw new TypeError("GameView launch requires at least one tracked surface");
  }
  const indices = new Set<number>();
  for (const surface of options.surfaces) {
    if (!(surface.attach instanceof HTMLElement)) {
      throw new TypeError(`surface ${surface.index} must attach to an HTMLElement`);
    }
    if (!Number.isInteger(surface.index) || surface.index < 0 || surface.index >= 16) {
      throw new RangeError(`surface index ${surface.index} is outside 0..=15`);
    }
    if (indices.has(surface.index)) throw new TypeError(`surface index ${surface.index} is duplicated`);
    indices.add(surface.index);
  }
}

function wireOptions(options: LaunchOptions): WireLaunchOptions {
  return {
    command: options.command,
    arguments: [...(options.arguments ?? [])],
    isolated: Boolean(options.isolated),
    hook: [...options.hook],
    surfaces: options.surfaces.map((surface) => ({
      index: surface.index,
      rotation: surface.rotation ?? 0,
      selector: surface.selector,
      attach: attachmentId(surface.attach),
      webrtc: Boolean(surface.webrtc),
    })),
    gamepadMapping: (options.gamepadMapping ?? []).map((mapping) => ({
      ...mapping,
      mapping: { ...(mapping.mapping ?? {}) },
    })),
    webrtcBind: options.webrtcBind,
  };
}

class GameViewInstanceImpl implements GameViewInstance {
  readonly id: string;
  readonly requested: readonly TrackedSurface[];
  #surfaces: readonly TrackedSurface[] = Object.freeze([]);
  #listeners = new Map<string, Set<(event: unknown) => void>>();
  #closed = false;

  constructor(id: string, requested: TrackedSurface[]) {
    this.id = id;
    this.requested = Object.freeze([...requested]);
  }

  get surfaces(): TrackedSurface[] {
    return [...this.#surfaces];
  }

  on(eventName: string, callback: (event: unknown) => void): void {
    if (typeof callback !== "function") throw new TypeError("event callback must be a function");
    let listeners = this.#listeners.get(eventName);
    if (!listeners) {
      listeners = new Set();
      this.#listeners.set(eventName, listeners);
    }
    listeners.add(callback);
  }

  message(messageName: string, args: unknown): void {
    if (this.#closed) return;
    host().message(JSON.stringify({ instanceId: this.id, name: messageName, args: args ?? null }));
  }

  close(): void {
    if (this.#closed) return;
    host().close(this.id);
    this.#closed = true;
  }

  nativeEvent(eventName: string, payload: unknown): void {
    if (eventName === "surfacesChanged") {
      const connected = new Set(
        Array.isArray(payload)
          ? (payload as NativeSurface[]).map((surface) => surface.index)
          : [],
      );
      this.#surfaces = Object.freeze(
        this.requested.filter((surface) => connected.has(surface.index)),
      );
      nativeSurfaces = Array.isArray(payload) ? (payload as NativeSurface[]) : [];
      applyNativeSurfaceMetadata(this);
      scheduleLayout();
    } else if (eventName === "closed") {
      this.#closed = true;
      this.#surfaces = Object.freeze([]);
      nativeSurfaces = [];
      manager.release(this);
      scheduleLayout();
    }
    for (const callback of this.#listeners.get(eventName) ?? []) {
      try {
        callback(payload);
      } catch (error) {
        console.error(`GameView '${eventName}' handler failed`, error);
      }
    }
  }
}

class GameViewManagerImpl implements GameViewManager {
  #active?: GameViewInstanceImpl;

  get activeInstance(): GameViewInstance | undefined {
    return this.#active;
  }

  launch(options: LaunchOptions): GameViewInstance | null {
    if (this.#active) return null;
    try {
      validateOptions(options);
      const id = String(host().launch(JSON.stringify(wireOptions(options))));
      if (!id) return null;
      const instance = new GameViewInstanceImpl(id, options.surfaces);
      this.#active = instance;
      observeAttachments(instance);
      scheduleLayout();
      return instance;
    } catch (error) {
      console.error("GameView launch failed", error);
      return null;
    }
  }

  activateInput(): void {
    if (this.#active) host().activateInput();
  }

  deactivateInput(): void {
    host().deactivateInput();
  }

  release(instance: GameViewInstanceImpl): void {
    if (this.#active === instance) {
      this.#active = undefined;
      nativeSurfaces = [];
    }
  }

  nativeEvent(event: NativeEvent): void {
    if (this.#active?.id === String(event.instanceId)) {
      this.#active.nativeEvent(event.eventName, event.payload);
    }
  }
}

const manager = new GameViewManagerImpl();
Object.defineProperty(window, "gameView", { value: manager, writable: false });
Object.defineProperty(window, "snowglassGameView", { value: manager, writable: false });
window.__snowglassGameViewNativeEvent = (event) => manager.nativeEvent(event);

let nativeSurfaces: NativeSurface[] = [];
let layoutScheduled = false;
let lastLayout = "";
const resizeObserver = new ResizeObserver(() => scheduleLayout());

function observeAttachments(instance: GameViewInstanceImpl): void {
  resizeObserver.disconnect();
  if (document.documentElement) resizeObserver.observe(document.documentElement);
  for (const surface of instance.requested) resizeObserver.observe(surface.attach);
}

function applyNativeSurfaceMetadata(instance: GameViewInstanceImpl): void {
  for (const native of nativeSurfaces) {
    const requested = instance.requested.find((surface) => surface.index === native.index);
    if (!requested) continue;
    requested.attach.dataset.connection = String(native.connection);
    requested.attach.dataset.index = String(native.index);
    if (requested.attach instanceof GameViewTargetElement) {
      requested.attach.setPreferredSize(native);
    }
  }
}

function autoFit(element: HTMLElement): void {
  if (!element.dataset.gameViewAutoFit) return;
  const parent = element.parentElement;
  const preferred =
    element instanceof GameViewTargetElement ? element.preferredSize : { width: 0, height: 0 };
  if (!parent || !preferred.width || !preferred.height) return;
  const bounds = parent.getBoundingClientRect();
  const fit = Math.min(bounds.width / preferred.width, bounds.height / preferred.height);
  const mode = element instanceof GameViewTargetElement ? element.scaling : "fit";
  const scale = mode === "integer" && fit >= 1 ? Math.max(1, Math.floor(fit)) : fit;
  const width = `${preferred.width * scale}px`;
  const height = `${preferred.height * scale}px`;
  if (element.style.width !== width) element.style.width = width;
  if (element.style.height !== height) element.style.height = height;
}

function effectiveZIndex(element: HTMLElement): number {
  let zIndex = 0;
  let depth = 0;
  for (let current: HTMLElement | null = element; current; current = current.parentElement) {
    const parsed = Number.parseInt(getComputedStyle(current).zIndex, 10);
    if (Number.isFinite(parsed)) zIndex += parsed * 1024;
    depth += 1;
  }
  return Math.max(-2_147_483_648, Math.min(2_147_483_647, zIndex + depth));
}

function acceptsPointerEvents(element: HTMLElement): boolean {
  const style = getComputedStyle(element);
  return style.pointerEvents !== "none" && style.visibility !== "hidden" && style.display !== "none";
}

function physicalRect(element: Element, scale: number) {
  const rect = element.getBoundingClientRect();
  return {
    x: Math.round(rect.left * scale),
    y: Math.round(rect.top * scale),
    width: Math.max(0, Math.round(rect.width * scale)),
    height: Math.max(0, Math.round(rect.height * scale)),
  };
}

function scheduleLayout(): void {
  if (layoutScheduled) return;
  layoutScheduled = true;
  requestAnimationFrame(publishLayout);
}

function publishLayout(): void {
  layoutScheduled = false;
  const instance = manager.activeInstance as GameViewInstanceImpl | undefined;
  if (!instance) return;
  const scale = window.devicePixelRatio || 1;
  const layouts = nativeSurfaces.flatMap((native) => {
    const surface = instance.requested.find((candidate) => candidate.index === native.index);
    if (!surface || !surface.attach.isConnected) return [];
    autoFit(surface.attach);
    const layout: Record<string, unknown> = {
      connection: String(native.connection),
      ...physicalRect(surface.attach, scale),
      zIndex: effectiveZIndex(surface.attach),
      pointerEvents: acceptsPointerEvents(surface.attach),
    };
    const foregroundHits = surface.attach.closest<HTMLElement>(
      "[data-gameview-foreground-hits]",
    );
    if (foregroundHits) {
      layout.foregroundHitRegions = [
        ...foregroundHits.querySelectorAll<HTMLElement>("[data-gameview-foreground-hit]"),
      ]
        .filter((element) => {
          const style = getComputedStyle(element);
          return (
            style.pointerEvents !== "none" &&
            style.visibility !== "hidden" &&
            style.display !== "none"
          );
        })
        .map((element) => physicalRect(element, scale));
    } else {
      const foregroundHit = surface.attach.closest<HTMLElement>(
        "[data-gameview-foreground-hit]",
      );
      if (foregroundHit) layout.foregroundHitRegion = physicalRect(foregroundHit, scale);
    }
    return [layout];
  });
  const serialized = JSON.stringify(layouts);
  if (serialized !== lastLayout) {
    lastLayout = serialized;
    window.ipc.postMessage(JSON.stringify({ type: "layout", layouts }));
  }
}

const domObserver = new MutationObserver(scheduleLayout);
function observeDocument(): void {
  if (!document.documentElement) return;
  domObserver.observe(document.documentElement, {
    attributes: true,
    attributeFilter: ["class", "style", "hidden"],
    childList: true,
    characterData: true,
    subtree: true,
  });
}
if (document.documentElement) {
  observeDocument();
} else {
  addEventListener("DOMContentLoaded", observeDocument, { once: true });
}
addEventListener("resize", scheduleLayout);
addEventListener("scroll", scheduleLayout, true);

let cliLaunchOptions: WireLaunchOptions | undefined;
Object.defineProperty(window, "snowglassCliLaunchOptions", {
  get: () => cliLaunchOptions,
});
addEventListener("message", (event) => {
  if (event.data?.type !== "snowglass-cli-launch") return;
  cliLaunchOptions = event.data.options as WireLaunchOptions;
  dispatchEvent(new CustomEvent("snowglassclilaunch", { detail: cliLaunchOptions }));
});

window.__snowglassSetInputState = (state, routed = []) => {
  inputState = Object.freeze([...state]);
  dispatchEvent(new CustomEvent("snowglassinputstate", { detail: inputState }));
  for (const event of routed) dispatchEvent(new CustomEvent("snowglassinput", { detail: event }));
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

function announceFrontendReady(): void {
  if (window.top === window && location.href !== "about:blank") {
    window.ipc.postMessage(JSON.stringify({ type: "frontendReady" }));
  }
}
if (document.readyState === "loading") {
  addEventListener("DOMContentLoaded", announceFrontendReady, { once: true });
} else {
  queueMicrotask(announceFrontendReady);
}
