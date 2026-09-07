export interface HTMLTrackedSurfaceElement extends HTMLElement {
  readonly preferredSize: Readonly<{ width: number; height: number }>;
}

export type GraphicsHook = "vulkan" | "d3d11" | "d3d12" | "opengl";
export type InputHook = "sdl3" | "bluetooth-api-win32";
export type ProjectionMode = "name" | "position" | "custom";
export type PointerMode = "none" | "relative" | "absoluteDevice" | "absoluteScreen";

export type InputSourceSelector =
  | { kind: "keyboardMouse" }
  | { kind: "device"; deviceId: string };

export type InputCalibration = Readonly<{
  sticks?: readonly (Readonly<{
    center: readonly [number, number];
    min: readonly [number, number];
    max: readonly [number, number];
    deadZone: number;
    outerDeadZone: number;
  }> | null)[];
  triggers?: readonly (Readonly<{ min: number; max: number; deadZone: number }> | null)[];
  motion?: Readonly<{
    accelerometerBias: readonly [number, number, number];
    accelerometerScale: readonly [number, number, number];
    gyroscopeBias: readonly [number, number, number];
    gyroscopeScale: readonly [number, number, number];
  }>;
  pointer?: Readonly<{
    sensorBar: "above" | "below";
    sensitivity: 1 | 2 | 3 | 4 | 5;
    mode: "basic" | "extended" | "full";
  }>;
}>;

export type InputState = Readonly<{
  buttons: number;
  leftStick: readonly [number, number];
  rightStick: readonly [number, number];
  leftTrigger: number;
  rightTrigger: number;
  accelerometer: readonly [number, number, number];
  gyroscope: readonly [number, number, number];
  orientation: readonly [number, number, number, number] | null;
  pointer: Readonly<{ position: readonly [number, number]; delta: readonly [number, number]; flags: number; kind: string }>;
  battery: Readonly<{ level: number; charging: boolean }>;
}>;

export interface InputCalibrationCapture {
  readonly progress: number;
  finish(): InputCalibration;
  cancel(): void;
}

export interface InputApi extends EventTarget {
  get(index: number): InputState | null;
}

export type ControllerDevice = {
  readonly id: string;
  readonly name: string;
  readonly profile: string | null;
  readonly vendorId: number | null;
  readonly productId: number | null;
  readonly serial: string | null;
  readonly role: "Whole" | "LeftHalf" | "RightHalf";
  readonly capabilities: readonly string[];
};

export type ManagedControllerDevice = ControllerDevice & Readonly<{
  index: number | null;
  connected: boolean;
  battery: Readonly<{ level: number | null; charging: boolean }> | null;
  pairing?: boolean;
}>;

export type ControllerPlayer = {
  readonly index: number;
  readonly controller: ManagedControllerDevice | null;
};

export type ControllerMappingProfile = {
  readonly mode: ProjectionMode;
  /** Logical input expression to physical input expression, or `none`. */
  readonly mapping: Readonly<Record<string, string>>;
  readonly scope: "device" | "type";
};

export type BluetoothAvailability = "unknown" | "available" | "unavailable";

export interface ConnectionSession extends EventTarget {
  readonly id: string;
  readonly active: boolean;
  readonly discovery: Readonly<{ active: boolean }>;
  readonly candidates: readonly ManagedControllerDevice[];
  readonly players: readonly ControllerPlayer[];
  connect(id: string): Promise<void>;
  commit(): Promise<void>;
}

export interface ControllersApi extends EventTarget {
  readonly availability: Readonly<{
    osBle: BluetoothAvailability;
    winUsb: BluetoothAvailability;
  }>;
  readonly devices: readonly ManagedControllerDevice[];
  readonly connected: readonly ManagedControllerDevice[];
  readonly players: readonly ControllerPlayer[];
  readonly layouts: readonly Readonly<Record<string, unknown>>[];
  readonly activeConnectionSession: ConnectionSession | null;
  startConnectionSession(): Promise<ConnectionSession>;
  rumble(deviceId: string, options: Readonly<{ lowFrequency: number; highFrequency: number; durationMs: number }>): void;
  recenter(deviceId: string): void;
  calibrate(deviceId: string, calibration: InputCalibration): InputCalibration;
  getCalibration(deviceId: string): InputCalibration;
  resetCalibration(deviceId: string): InputCalibration;
  beginCalibration(deviceId: string, kind: "stickCenter" | "stickRange" | "triggerRange" | "motionBias"): InputCalibrationCapture;
  configurePointer(deviceId: string, pointer: NonNullable<InputCalibration["pointer"]>): InputCalibration;
  disconnect(id: string): Promise<void>;
  forget(id: string): Promise<void>;
  getMapping(id: string, layoutId: string): Promise<ControllerMappingProfile>;
  setMapping(
    id: string,
    layoutId: string,
    mapping: Readonly<{ mode: ProjectionMode; mapping?: Readonly<Record<string, string>> }>,
  ): Promise<ControllerMappingProfile>;
  resetMapping(id: string, layoutId: string): Promise<ControllerMappingProfile>;
}

export type TrackedSurface = {
  index: number;
  rotation: number;
  selector?: string;
  resolution?: Resolution;
  attach: HTMLTrackedSurfaceElement;
  webrtc: boolean;
};

export type ControllerMapping = {
  target: Readonly<{ group: string; port: number }>;
  gamepad: number;
  source?: InputSourceSelector;
  connected: boolean;
  device?: ControllerDevice;
  mouse: boolean;
  layout: string;
  projectionMode?: ProjectionMode;
  /** Logical input expression to physical input expression, or `none`. */
  mapping: Record<string, string>;
  pointer: Readonly<{
    source: "none" | "deviceNative" | "hostMouse" | "deviceThenHostMouse";
    mode: PointerMode;
  }>;
};

export type ControllerTopology = Readonly<{
  mappings: readonly ControllerMapping[];
}>;

export type EmulatorSurface = {
  index: number;
  pattern?: string;
  rotation?: number;
  resolution?: Resolution;
};
export type EmulatorRenderSelection = RenderSelection<EmulatorSurface>;

export type Resolution = readonly [number, number];
export type RenderSelection<S> = {
  hooks: { graphics: GraphicsHook[] };
  surfaces: (S & { resolution: Resolution })[];
};

export type LaunchOptions = LaunchFields & RenderSelection<TrackedSurface>;

type LaunchFields = {
  command: string;
  arguments: string[];
  isolated: boolean;
  hooks: Readonly<{
    graphics: GraphicsHook[];
    input: InputHook[];
  }>;
  controllerTopology: ControllerTopology;
  machine: string;
  webrtcBind?: string;
  preparedFilesystem?: string;
  controllerSession?: string;
  saveStates?: Readonly<{ max: number; revision?: string }> | null;
  quickResume?: Readonly<{ screenshot: Blob | null }> | null;
  quickResumeCache?: Readonly<{ key: string; revision: string }> | null;
};

export type LaunchRequest = LaunchFields & RenderSelection<Omit<TrackedSurface, "attach">>;

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
  readonly saveStates: readonly SaveState[];
  readonly quickResume: Readonly<{ screenshot: Blob | null }> | null;
  send(messageName: string, args?: unknown): void;
  setVolume(volume: number): void;
  setMuted(muted: boolean): void;
  activateInput(): void;
  deactivateInput(): void;
  setPauseState(pauseState: "pause" | "resume", timeoutMs?: number): Promise<void>;
  saveState(timeoutMs?: number): Promise<SaveState>;
  resumeState(slot: number, timeoutMs?: number): Promise<SaveState>;
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

export interface PluginApi extends EventTarget {
  /** Latest events per runtime source and topic, bounded to 256 entries for late listeners. */
  events(): readonly PluginEvent[];
  open(id: string): PluginExport;
  list(): readonly Readonly<{ id: string; version: string; class: string }>[];
}

export type PluginEvent<T = unknown> = Readonly<{
  sourceId: number;
  pluginId: string;
  name: string;
  detail: T;
}>;

export type PluginLoadProgress = Readonly<{
  id: number;
  pluginId: string;
  state: "loading" | "ready" | "failed";
  message: string;
  completed: number | null;
  total: number | null;
}>;

import type { Package as PackageData } from "./package.ts";

export interface Package extends PackageData {
  toJSON(): PackageData;
}

export interface SaveState {
  readonly slot: number;
  readonly timestamp: Date;
  screenshot(): Promise<Blob | null>;
}

export interface ScrapingApi extends EventTarget {
  readonly Package: {
    readonly prototype: Package;
    fromPath(path: string, options?: { signal?: AbortSignal }): Promise<Package>;
  };
  readonly dropActive: boolean;
  readonly jobs: Readonly<{
    list(): Promise<ScrapeJob[]>;
    get(id: string): Promise<ScrapeJob | null>;
    start(rootBinaryId: string, options?: { triggerBinaryId?: string }): Promise<ScrapeJob>;
    retry(id: string): Promise<ScrapeJob>;
  }>;
  readonly scrapers: Readonly<{
    list(): Promise<ScraperDescriptor[]>;
    update(id: string): Promise<void>;
  }>;
}

export type ScrapeJobState = "queued" | "running" | "completed" | "partial" | "failed";
export type ScrapeAttemptState = "queued" | "running" | "completed" | "failed";
export type ScrapeJob = Readonly<{
  id: string;
  rootBinaryId: string;
  triggerBinaryId: string;
  state: ScrapeJobState;
  createdAt: number;
  updatedAt: number;
  attempts: readonly Readonly<{ pluginId: string; state: ScrapeAttemptState; error: string | null }>[];
}>;
export type ScraperDescriptor = Readonly<{ id: string; version: string; requires: readonly string[]; provides: readonly string[] }>;

export type {
  ConfigurationChoice,
  ConfigurationGroup,
  ConfigurationLayout,
  ConfigurationLayoutOption,
  UserConfiguration,
} from "./configuration.ts";
import type { ConfigurationLayout, UserConfiguration } from "./configuration.ts";

export type ConfigurationPort = {
  index: number;
  layouts: string[];
  selectedLayout: string;
};

export type ConfigurationPortGroup = {
  name: string;
  connected: number;
  ports: ConfigurationPort[];
};

export type EmulatorConfiguration = {
  id: string;
  groups: ConfigurationPortGroup[];
  configuration: ConfigurationLayout | null;
  userConfiguration: UserConfiguration;
};

export type ConfigurationState = Readonly<
  Pick<EmulatorConfiguration, "configuration" | "userConfiguration">
>;

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
      mapping?: Record<string, string>;
    }>;
  }>;
  storage?: Record<string, string | string[]>;
};

export interface ConfigurationApi {
  describe(launchableId: string, options?: { emulator?: string }): Promise<PackageConfiguration>;
  configure(
    launchableId: string,
    emulator: string,
    userConfiguration?: UserConfiguration,
  ): Promise<ConfigurationState | null>;
  prepare(launchableId: string, selection: ConfigurationSelection): Promise<unknown>;
  onLaunch(launchableId: string, selection: ConfigurationSelection): Promise<LaunchRequest>;
}

export type ConsoleStorageKind = "nand" | "saveRepo" | "memoryCard" | "battery";
export type CreatableConsoleStorageKind = Exclude<ConsoleStorageKind, "battery">;

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
    supports(kind: ConsoleStorageKind, platform: string): Promise<boolean>;
    create(kind: CreatableConsoleStorageKind, platform: string, options?: { label?: string }): Promise<ConsoleStorageVolume>;
    list(options?: { kind?: ConsoleStorageKind }): Promise<ConsoleStorageVolume[]>;
  };
}

export type LibraryMetadataInput = Readonly<{
  key: string;
  source?: string;
  data:
    | Readonly<{ kind: "scalar"; value: string | number | boolean }>
    | Readonly<{ kind: "inline"; mediaType: string; bytes: Uint8Array }>
    | Readonly<{ kind: "object"; mediaType: string; path: string }>;
}>;

export type LibraryLaunchable = Readonly<{
  id: string;
  binaryId: string;
  path: string;
  binaryBlake3: string;
  serial: string | null;
  titleId: string | null;
  parentBinaryId: string | null;
  platform: string;
}>;

export type LibraryMetadata = Readonly<{
  id: string;
  parentBinaryId: string;
  key: string;
  source: string;
  producer: string;
  ordinal: number;
  data:
    | Readonly<{ kind: "scalar"; value: string | number | boolean }>
    | Readonly<{ kind: "media"; mediaType: string; storage: "inline" | "object"; sourceUrl: string | null }>;
}>;

export type LibraryGame = Readonly<{
  id: string;
  platform: string;
  launchables: readonly LibraryLaunchable[];
  metadata: readonly LibraryMetadata[];
}>;

export interface LibraryApi extends EventTarget {
  readonly games: Readonly<{
    list(options?: { platform?: string }): Promise<LibraryGame[]>;
    get(rootBinaryId: string): Promise<LibraryGame | null>;
    delete(rootBinaryId: string): Promise<void>;
  }>;
  readonly launchables: Readonly<{
    add(input: { path: string; platform?: string; parentBinaryId?: string }): Promise<Readonly<{ launchable: LibraryLaunchable; scrapeJob: ScrapeJob | null }>>;
    delete(id: string): Promise<void>;
  }>;
  readonly metadata: Readonly<{
    add(rootBinaryId: string, input: LibraryMetadataInput & { source: string }): Promise<LibraryMetadata>;
    update(id: string, input: LibraryMetadataInput & { source: string }): Promise<LibraryMetadata>;
    delete(id: string): Promise<void>;
    replace(rootBinaryId: string, source: string, inputs: readonly LibraryMetadataInput[]): Promise<LibraryMetadata[]>;
    text(id: string): Promise<string>;
    url(id: string): Promise<string>;
  }>;
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
    readonly Polymer: Readonly<{
      readonly scraping: ScrapingApi;
      readonly configuration: ConfigurationApi;
      readonly consolestorage: ConsoleStorageApi;
      readonly emulator: EmulatorApi;
      readonly plugin: PluginApi;
      readonly webrtc: WebRtcApi;
      readonly input: InputApi;
      readonly controllers: ControllersApi;
      readonly library: LibraryApi;
    }>;
  }

  interface HTMLElementTagNameMap {
    "tracked-surface": HTMLTrackedSurfaceElement;
  }
}
