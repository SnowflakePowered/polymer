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
  slot: number;
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

declare global {
  interface Window {
    readonly Emulator: EmulatorApi;
    readonly gameLayer: EventTarget & {
      readonly contentWindow: BackgroundWindowProxy;
      readonly ready: boolean;
      postMessage(message: unknown): void;
    };
  }

  interface HTMLLinkElement {
    readonly contentWindow: BackgroundWindowProxy | null;
  }

  interface HTMLElementTagNameMap {
    "tracked-surface": HTMLTrackedSurfaceElement;
  }
}
