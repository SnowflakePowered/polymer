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
  machine: string;
  webrtcBind?: string;
  generatedConfiguration?: {
    root: string;
    files: Record<string, number[]>;
  };
  preparedFilesystem?: string;
};

export type LaunchRequest = Omit<LaunchOptions, "surfaces"> & {
  surfaces: Array<Omit<TrackedSurface, "attach">>;
};

export type LaunchState = "pending" | "starting" | "ready" | "closing" | "closed" | "error";

export interface LaunchInstance extends EventTarget {
  readonly id: number;
  readonly state: LaunchState;
  readonly error: Error | null;
  readonly request: Readonly<LaunchOptions>;
  readonly ready: Promise<void>;
  readonly closed: Promise<void>;
  readonly surfaces: readonly TrackedSurface[];
  readonly audio: Readonly<{ available: boolean; volume: number; muted: boolean }>;
  send(messageName: string, args?: unknown): void;
  setVolume(volume: number): void;
  setMuted(muted: boolean): void;
  activateInput(): void;
  deactivateInput(): void;
  close(): void;
}

export interface EmulatorApi extends EventTarget {
  readonly activeLaunch: LaunchInstance | null;
  readonly LaunchInstance: { prototype: LaunchInstance };
  launch(options: LaunchOptions): LaunchInstance;
}

export interface PluginExport {
  (...args: unknown[]): Promise<any>;
  readonly [property: string]: PluginExport;
}

export interface PluginApi {
  open(id: string): PluginExport;
}

export interface ScrapedPackage {
  readonly path: string;
  readonly name: string;
  readonly platform: string;
  toJSON(): Readonly<{ path: string; name: string; platform: string }>;
}

export interface ScrapingApi extends EventTarget {
  readonly Package: {
    readonly prototype: ScrapedPackage;
    fromPath(path: string, options?: { signal?: AbortSignal }): Promise<ScrapedPackage>;
  };
  readonly dropActive: boolean;
}

export type ConfigurationOption = {
  path: string;
  title: string;
  kind: "boolean" | "integer" | "float" | "string" | "path" | "select";
  value: unknown;
  min?: number;
  max?: number;
  choices?: Array<{ key: string; label: string }>;
};

export type ConfigurationPort = {
  index: number;
  layouts: string[];
  selectedLayout: string;
  selectedDevice: string;
};

export type ConfigurationPortGroup = {
  name: string;
  connected: number;
  ports: ConfigurationPort[];
};

export type EmulatorConfiguration = {
  id: string;
  groups: ConfigurationPortGroup[];
  options: ConfigurationOption[];
};

export type PackageConfiguration = {
  path: string;
  name: string;
  platform: string;
  selectedEmulator: string;
  candidates: EmulatorConfiguration[];
};

export type ConfigurationSelection = {
  emulator: string;
  groups: Array<{
    name: string;
    ports: Array<{ index: number; layout: string; device: string }>;
  }>;
  settings: Record<string, unknown>;
  nand?: string;
};

export interface ConfigurationApi {
  describe(packageValue: ScrapedPackage, options?: { emulator?: string }): Promise<PackageConfiguration>;
  prepare(packageValue: ScrapedPackage, selection: ConfigurationSelection): Promise<unknown>;
  setupLaunch(packageValue: ScrapedPackage, selection: ConfigurationSelection): Promise<LaunchRequest>;
}

export type WebRtcPublicationState = "starting" | "active" | "closing" | "closed" | "error";

export interface WebRtcPublication extends EventTarget {
  readonly instance: LaunchInstance;
  readonly bind: string;
  readonly targets: readonly number[];
  readonly state: WebRtcPublicationState;
  readonly error: Error | null;
  close(): Promise<void>;
}

export interface WebRtcApi extends EventTarget {
  readonly activePublication: WebRtcPublication | null;
  start(options: {
    instance: LaunchInstance;
    bind?: string;
    targets: number[];
  }): Promise<WebRtcPublication>;
}

declare global {
  interface Window {
    readonly Snowglass: Readonly<{
      readonly scraping: ScrapingApi;
      readonly configuration: ConfigurationApi;
      readonly emulator: EmulatorApi;
      readonly plugin: PluginApi;
      readonly webrtc: WebRtcApi;
      readonly input: EventTarget & {
        readonly configuration: unknown;
        readonly layouts: unknown;
        readonly controllers: unknown;
        readonly priority: "ui" | "game";
        configure(transaction: unknown): Promise<unknown>;
      };
    }>;
  }

  interface HTMLElementTagNameMap {
    "tracked-surface": HTMLTrackedSurfaceElement;
  }
}
