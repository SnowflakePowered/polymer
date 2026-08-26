export interface HTMLTrackedSurfaceElement extends HTMLElement {
  readonly preferredSize: Readonly<{ width: number; height: number }>;
}

export type GraphicsHook = "opengl" | "vulkan" | "d3d11" | "d3d12";
export type ProjectionMode = "name" | "position" | "custom";

export type InputSourceSelector =
  | { kind: "keyboardMouse" }
  | { kind: "device"; deviceId: string };

export type InputSourceAssignment = {
  readonly index: number;
  readonly source: Readonly<InputSourceSelector>;
};

export type InputSource = {
  readonly id: string;
  readonly kind: "controller" | "keyboardMouse";
  readonly name: string;
  readonly connected: boolean;
  readonly assignedIndices: readonly number[];
  readonly layout?: string;
  readonly buttons: readonly Readonly<{ button: string; pressed: boolean; mapped: boolean }>[];
  readonly axes: readonly Readonly<{ axis: string; value: number; mapped: boolean }>[];
  readonly mapped?: Readonly<Record<string, unknown>>;
};

export type VirtualInputPort = {
  readonly port: string;
  readonly layout: string;
  readonly slot: number | null;
  readonly forceMouse: boolean;
  readonly projectionMode: ProjectionMode;
  readonly overrides: Readonly<Record<string, unknown>>;
};

export type InputConfigurationSnapshot = {
  readonly revision: number;
  readonly machine: string | null;
  readonly ports: readonly VirtualInputPort[];
  readonly assignments: readonly InputSourceAssignment[];
};

export type InputControlEvent = {
  readonly sourceId: string;
  readonly controlType: "button" | "axis";
  readonly control: string;
  readonly value: number;
};

export interface InputApi extends EventTarget {
  readonly configuration: InputConfigurationSnapshot;
  readonly revision: number;
  readonly ports: readonly VirtualInputPort[];
  readonly assignments: readonly InputSourceAssignment[];
  readonly layouts: readonly Readonly<Record<string, unknown>>[];
  readonly sources: readonly InputSource[];
  readonly controllers: readonly InputSource[];
  readonly priority: "ui" | "game";
  getControllers(): readonly InputSource[];
  getGamepads(): readonly Readonly<Record<string, unknown>>[];
  getControllerLayout(id: string): Readonly<Record<string, unknown>> | null;
  assign(assignments: readonly { index: number; sourceId: string }[]): Promise<InputConfigurationSnapshot>;
  configure(transaction: Readonly<Record<string, unknown>>): Promise<InputConfigurationSnapshot>;
}

export type TrackedSurface = {
  index: number;
  rotation: number;
  selector?: string;
  attach: HTMLTrackedSurfaceElement;
  webrtc: boolean;
};

export type GamePadMapping = {
  source?: InputSourceSelector;
  mouse: boolean;
  slot: number;
  port?: string;
  layout: string;
  projectionMode?: ProjectionMode;
  mapping: Record<string, string>;
};

export type LaunchOptions = {
  command: string;
  arguments: string[];
  isolated: boolean;
  hook: GraphicsHook[];
  graphTracking: boolean;
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
  readonly serial: string | null;
  readonly titleId: string | null;
  toJSON(): Readonly<{
    path: string;
    name: string;
    platform: string;
    serial: string | null;
    titleId: string | null;
  }>;
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
  projectionMode: ProjectionMode;
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
    ports: Array<{
      index: number;
      layout: string;
      source?: InputSourceSelector;
      projectionMode?: ProjectionMode;
      mapping?: Record<string, string>;
    }>;
  }>;
  settings: Record<string, unknown>;
  storage?: Record<string, string | string[]>;
};

export interface ConfigurationApi {
  describe(packageValue: ScrapedPackage, options?: { emulator?: string }): Promise<PackageConfiguration>;
  prepare(packageValue: ScrapedPackage, selection: ConfigurationSelection): Promise<unknown>;
  setupLaunch(packageValue: ScrapedPackage, selection: ConfigurationSelection): Promise<LaunchRequest>;
}

export type ConsoleStorageKind = "nand" | "saveRepo" | "memoryCard";

export interface ConsoleStorageVolume extends EventTarget {
  readonly kind: ConsoleStorageKind;
  readonly id: string;
  readonly platform: string;
  readonly label: string | null;
  readonly created: Date;
  readonly path: string;
  setLabel(label: string | null): Promise<ConsoleStorageVolume>;
  delete(): Promise<void>;
}

export interface ConsoleStorageApi {
  readonly Storage: {
    readonly prototype: ConsoleStorageVolume;
    create(kind: ConsoleStorageKind, platform: string, options?: { label?: string }): Promise<ConsoleStorageVolume>;
    list(options?: { kind?: ConsoleStorageKind }): Promise<ConsoleStorageVolume[]>;
  };
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
      readonly consolestorage: ConsoleStorageApi;
      readonly emulator: EmulatorApi;
      readonly plugin: PluginApi;
      readonly webrtc: WebRtcApi;
      readonly input: InputApi;
    }>;
  }

  interface HTMLElementTagNameMap {
    "tracked-surface": HTMLTrackedSurfaceElement;
  }
}
