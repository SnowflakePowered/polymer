export interface HTMLTrackedSurfaceElement extends HTMLElement {
  readonly preferredSize: Readonly<{ width: number; height: number }>;
}

export type GraphicsHook = "opengl" | "vulkan" | "d3d11" | "d3d12";

export type TrackedSurface = {
  index: number;
  rotation: number;
  selector?: string;
  attach: HTMLTrackedSurfaceElement;
  webrtc: boolean;
};

export type GamePadMapping = {
  gamepadId: number | "K" | string;
  mouse: boolean;
  // The virtual pad this drives, dense from zero.
  slot: number;
  // The emulator-facing port it stands in for, e.g. "controller.0". Optional:
  // when the machine has exactly one group accepting `layout` — which is true
  // of every machinespec today — the host resolves it.
  port?: string;
  layout: string;
  mapping: Record<string, string>;
};

export type LaunchOptions = {
  command: string;
  arguments: string[];
  isolated: boolean;
  hook: GraphicsHook[];
  surfaces: TrackedSurface[];
  gamepadMapping: GamePadMapping[];
  // The consolespec machine being emulated. Its machinespec is what decides
  // which ports exist and which layouts may occupy them, so a launch cannot
  // describe its input without naming it.
  machine: string;
  webrtcBind?: string;
};

export type LaunchRequest = Omit<LaunchOptions, "surfaces"> & {
  surfaces: Array<Omit<TrackedSurface, "attach">>;
};

export type EmulatorState =
  | "starting"
  | "ready"
  | "closing"
  | "closed"
  | "error";

export interface EmulatorInstance {
  readonly id: number;
  readonly state: EmulatorState;
  readonly error: Error | null;
  on(eventName: string, callback: (event: unknown) => void): void;
  off(eventName: string, callback: (event: unknown) => void): void;
  readonly surfaces: readonly TrackedSurface[];
  readonly audio: Readonly<{
    available: boolean;
    volume: number;
    muted: boolean;
  }>;
  send(messageName: string, args?: unknown): void;
  setVolume(volume: number): void;
  setMuted(muted: boolean): void;
  activateInput(): void;
  deactivateInput(): void;
  close(): void;
}

export interface EmulatorApi extends EventTarget {
  readonly activeInstance: EmulatorInstance | null;
  launch(options: LaunchOptions): EmulatorInstance | null;
}

/**
 * An asynchronous reference into the background Deno realm.
 *
 * Chained property reads and method calls are sent to the background. Await
 * the final read or call to obtain its value.
 */
export type BackgroundRemoteValue = {
  (...args: unknown[]): Promise<unknown>;
  readonly [property: string]: BackgroundRemoteValue;
};

export type BackgroundWindowProxy = BackgroundRemoteValue & {
  postMessage(message: unknown): void;
};

export type LauncherOption = {
  path: string;
  title: string;
  kind: "boolean" | "integer" | "float" | "string" | "path" | "select";
  value: unknown;
  min?: number;
  max?: number;
  choices?: Array<{ key: string; label: string }>;
};

export type LauncherPort = {
  index: number;
  layouts: string[];
  selectedLayout: string;
  selectedDevice: string;
};

export type LauncherPortGroup = {
  name: string;
  connected: number;
  ports: LauncherPort[];
};

export type LauncherEmulator = {
  id: string;
  groups: LauncherPortGroup[];
  options: LauncherOption[];
};

export type LauncherGame = {
  generation: number;
  path: string;
  name: string;
  platform: string;
  selectedEmulator: string;
  candidates: LauncherEmulator[];
};

export type LauncherState =
  | { kind: "idle" }
  | { kind: "detecting"; path: string; name: string }
  | { kind: "configure"; game: LauncherGame }
  | { kind: "launching"; name: string }
  | { kind: "error"; message: string };

export type LauncherStartRequest = {
  generation: number;
  emulator: string;
  groups: Array<{
    name: string;
    ports: Array<{ index: number; layout: string; device: string }>;
  }>;
  settings: Record<string, unknown>;
};

export interface LauncherApi extends EventTarget {
  readonly state: LauncherState;
  open(path: string, emulator?: string): void;
  start(request: LauncherStartRequest): void;
  cancel(): void;
}

declare global {
  interface Window {
    readonly Emulator: EmulatorApi;
    readonly gameLayer: EventTarget & {
      readonly contentWindow: BackgroundWindowProxy;
      readonly ready: boolean;
      postMessage(message: unknown): void;
    };
    readonly snowglassLauncher: LauncherApi;
  }

  interface HTMLLinkElement {
    readonly contentWindow: BackgroundWindowProxy | null;
  }

  interface HTMLElementTagNameMap {
    "tracked-surface": HTMLTrackedSurfaceElement;
  }
}
