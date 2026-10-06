export interface HTMLTrackedSurfaceElement extends HTMLElement {
  readonly preferredSize: Readonly<{ width: number; height: number }>;
}

export type GraphicsHook = "vulkan" | "d3d11" | "d3d12" | "opengl";
export type InputHook = "sdl2" | "sdl3" | "bluetooth-api-win32";
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
};
export type EmulatorRenderSelection = RenderSelection<EmulatorSurface>;

export type Resolution = readonly [number, number];
export type RenderSelection<S> = {
  hooks: { graphics: GraphicsHook[] };
  surfaces: S[];
};

export type LaunchOptions = LaunchFields & RenderSelection<TrackedSurface>;

type LaunchFields = {
  command: string;
  /** Native path, obtained with filesystem.path() for a virtual directory. */
  workingDirectory?: string;
  /** Environment overrides as [name, value] pairs. */
  env?: readonly (readonly [string, string])[];
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
  launchSet?: readonly LibraryLaunchable[];
  initialDiscIndex?: number;
  canChangeDisc?: boolean;
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
  readonly canChangeDisc: boolean;
  /** Last successfully selected disc; initialized before the emulator starts. */
  readonly currentDiscIndex: number;
  /** Immutable snapshot, in the same order supplied to configuration.onLaunch. */
  getLaunchSet(): readonly LibraryLaunchable[];
  /** Resolves after onDiscChange completes; rejects if unavailable, not ready, or invalid. */
  changeDisc(index: number): Promise<void>;
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

import type { Package as PackageData, Digestible } from "./package.ts";

export interface Package extends PackageData, Digestible {
  readonly binary: Digestible;
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
  start(rootBinaryId: string, options?: { triggerBinaryId?: string }): Promise<Job>;
  readonly scrapers: Readonly<{
    list(): Promise<ScraperDescriptor[]>;
    update(id: string): Promise<Job>;
  }>;
}

export type JobKind = "scrape" | "install" | "bootstrap" | "plugin" | "import" | "check" | "update" | "download" | "refresh";
export type JobState = "queued" | "running" | "paused" | "completed" | "partial" | "failed" | "cancelled";
export type JobStep = Readonly<{ key: string; state: JobState; error: string | null }>;
/** Background work from any source. Persistent jobs survive restarts and resume. */
export type Job = Readonly<{
  id: string;
  kind: JobKind;
  state: JobState;
  parent: string | null;
  title: string;
  icon: string | null;
  subject: Readonly<{ platform?: string; storage?: string; binaryId?: string; pluginId?: string }>;
  params: unknown;
  message: string | null;
  completed: number | null;
  total: number | null;
  /** Kind-specific: a StorageInstallStatus while installing, the StorageArtifact once installed. */
  detail: unknown;
  error: string | null;
  persistent: boolean;
  createdAt: number;
  updatedAt: number;
  /** For scrapes, one step per scraper keyed by plugin id. */
  steps: readonly JobStep[];
}>;

export interface JobsApi extends EventTarget {
  /** Every known job, newest first; `jobchange` fires with each updated job. */
  list(): Promise<Job[]>;
  get(id: string): Promise<Job | null>;
  /** Cancels a queued or paused job. */
  cancel(id: string): Promise<Job>;
  /** Starts a new job with a finished job's parameters. */
  retry(id: string): Promise<Job>;
  /** Removes a finished job. */
  dismiss(id: string): Promise<void>;
}

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
  /** The emulator chosen for this game, or null to follow the platform setting. */
  gameEmulator?: string | null;
  /** The platform's default emulator, or null to use the first candidate. */
  platformEmulator?: string | null;
  candidates: EmulatorConfiguration[];
};

export type PlatformSettings = { emulator: string | null };

export type PlatformConfiguration = {
  platform: string;
  /** Emulator plugins supporting this platform, in default preference order. */
  emulators: string[];
  settings: PlatformSettings;
};

export type ConfigurationSelection = {
  initialDiscIndex?: number;
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
  /** Persists the emulator for a launchable; null follows the platform setting. */
  select(launchableId: string, emulator: string | null): Promise<void>;
  readonly platforms: {
    list(): Promise<PlatformConfiguration[]>;
    update(platform: string, settings: PlatformSettings): Promise<PlatformConfiguration>;
  };
  prepare(launchableId: string, selection: ConfigurationSelection): Promise<unknown>;
  onLaunch(launchableId: string | readonly string[], selection: ConfigurationSelection): Promise<LaunchRequest>;
}

export type ConsoleStorageKind = "nand" | "saveRepo" | "memoryCard" | "battery";
export type CreatableConsoleStorageKind = Exclude<ConsoleStorageKind, "battery">;
export type DepotType = "system" | "app" | "appcontent" | "appupdate";

export interface StorageArtifact {
  id: string; label: string; version: string | null; depotType: DepotType; byteLength: number;
  installed: string; fileCount: number;
  /** Data URLs from the plainest image to the richest animated banner. */
  icons: string[];
  /** Install root within the NAND, present when the artifact is launchable. */
  root?: string;
  /** Launch file relative to `root`. */
  entrypoint?: string;
  /** NAND path of the installed package used for identification and scraping. */
  package?: string;
}
export type UpdateCandidate = Readonly<{
  titleId: string;
  version: string;
  label: string;
  byteLength: number | null;
  /** Platform-plugin-defined payload, returned unchanged when fetching. */
  source: unknown;
}>;
export interface StorageInspection { id: string; platform: string; minimal: boolean; dirty: boolean; artifacts: StorageArtifact[] }
export interface StorageRemovalPlan { requested: string[]; affected: string[]; revision: string }
export interface StorageSave {
  id: string; title: string; description: string; modified: string; icon: string | null;
  byteLength: number; blocks: number | null; versions: number;
}
export type SaveCommitOperation = "initialImport" | "autosave" | "manual" | "restore";
export interface StorageSaveCommit { id: string; parent: string | null; timestamp: string; operation: SaveCommitOperation; screenshot: string | null }
export interface ConsoleStorageVolume extends EventTarget {
  inspect(): Promise<StorageInspection>;
  /** Discard uncommitted NAND changes and restore the last successful commit. */
  recover(): Promise<StorageInspection>;
  install(source: { path: string }): Promise<StorageArtifact>;
  bootstrap(settings?: Record<string, unknown>): Promise<StorageInspection>;
  readonly artifacts: {
    list(): Promise<StorageArtifact[]>;
    planRemoval(ids: string[]): Promise<StorageRemovalPlan>;
    uninstall(plan: StorageRemovalPlan): Promise<StorageArtifact[]>;
  };
  readonly saves: {
    list(): Promise<StorageSave[]>;
    history(save: string): Promise<StorageSaveCommit[]>;
    restore(save: string, commit: string): Promise<StorageSaveCommit>;
    copyTo(save: string, destination: ConsoleStorageVolume): Promise<StorageSaveCommit>;
    delete(save: string): Promise<void>;
  };
  fork(label?: string | null): Promise<ConsoleStorageVolume>;
  readonly kind: ConsoleStorageKind;
  readonly id: string;
  readonly platform: string;
  readonly label: string | null;
  readonly created: Date;
  readonly path: string;
  setLabel(label: string | null): Promise<ConsoleStorageVolume>;
  delete(): Promise<void>;
}

export type InstallPhase = "preparing" | "verifying" | "installing" | "finalizing";

export type StorageInstallStatus = Readonly<{
  title: Readonly<{ id: string; label: string; depotType: DepotType; icon: string | null }> | null;
  phase: InstallPhase; file: string | null; completed: number; total: number;
}>;

export interface ConsoleStorageApi extends EventTarget {
  platforms(): Promise<{ platform: string; plugin: string; updates: boolean }[]>;
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

/** A launchable as plain data, the way it appears inside job details. */
export type LibraryLaunchableData = Readonly<{
  preparation?: Readonly<{ state: "pending" | "ready" | "failed"; error: string | null }> | null;
  id: string;
  binaryId: string;
  uri: string;
  serial: string | null;
  titleId: string | null;
  parentBinaryId: string | null;
  platform: string;
}>;

export type LibraryLaunchable = Digestible & LibraryLaunchableData & Readonly<{ binary: Digestible }>;

export type LibraryMetadata = Readonly<{
  id: string;
  parentBinaryId: string;
  key: string;
  source: string;
  producer: string;
  ordinal: number;
  data:
    | Readonly<{ kind: "scalar"; value: string | number | boolean }>
    | Readonly<{
      kind: "media"; mediaType: string; storage: "inline" | "object"; sourceUrl: string | null;
      width: number | null; height: number | null;
      blurhash: string | null;
    }>;
}>;

export type LibraryGame = Readonly<{
  id: string;
  platform: string;
  title: string;
  /** Each launchable carries the update candidates its last check found, oldest first. */
  launchables: readonly (LibraryLaunchable & Readonly<{ updates: readonly UpdateCandidate[] }>)[];
  metadata: readonly LibraryMetadata[];
}>;

export type LibraryGameQuery = Readonly<{
  text?: string;
  platforms?: readonly string[];
  sort?: "title" | "platform" | "added";
  descending?: boolean;
  offset?: number;
  limit?: number;
  /** Only these metadata keys are returned for each game. */
  metadataKeys?: readonly string[];
}>;

export type FirmwareDescription = Readonly<{
  platform: string;
  label: string;
  names: readonly string[];
  byteLength: number;
}>;

export type GameImport = Readonly<{ launchable: LibraryLaunchable; job: Job | null }>;
/** What an import found and applied, as `detail.result` of its completed `import` job. */
export type LibraryImportResult =
  | Readonly<{ kind: "games"; imports: readonly Readonly<{ launchable: LibraryLaunchableData; job: Job | null }>[] }>
  | Readonly<{ kind: "firmware"; firmware: FirmwareDescription }>
  | Readonly<{ kind: "installables"; installables: readonly InstallablePackage[] }>
  | Readonly<{ kind: "system"; platform: string; path: string; filename: string; version: string }>
  | Readonly<{ kind: "keys"; platform: string; count: number }>;

export type InstallablePackage = Readonly<{ path: string; filename: string; platform: string; titleId: string | null }>;

export type ImportedKey = Readonly<{ namespace: "system" | "content"; id: string; titleId?: string; byteLength: number }>;

export interface LibraryApi extends EventTarget {
  /**
   * Starts an `import` job for a path, or returns the active one for the same path. Installable
   * packages install into `storage`, or into the first NAND for their platform, created if needed.
   */
  import(input: { path: string; storage?: string }): Promise<Job>;
  readonly updates: Readonly<{
    /**
     * Starts an in-memory `check` job for a library title, installed or not; when it
     * completes, its detail holds `candidates`.
     */
    check(launchableId: string): Promise<Job>;
    /**
     * Starts an `update` job that downloads one candidate as a depot. When it completes, its
     * detail holds the `depot` path, a file or directory, and the id of the child `install`
     * job it queued.
     */
    fetch(launchableId: string, candidate: UpdateCandidate): Promise<Job>;
  }>;
  importKey(platform: string, namespace: "system" | "content", key: Uint8Array | string, titleid?: string): Promise<ImportedKey>;
  readonly keys: Readonly<{
    list(platform: string, options?: { namespace?: "system" | "content" }): Promise<ImportedKey[]>;
    platforms(): Promise<string[]>;
    /** False only when the title needs a license and no key for it has been imported. */
    licensed(platform: string, titleId: string): Promise<boolean>;
    remove(platform: string, namespace: "system" | "content", id: string): Promise<boolean>;
  }>;
  readonly games: Readonly<{
    /**
     * Pages through root games. Without `text`, games come in `sort` order; with `text`, by
     * fuzzy title match quality. `total` counts every game matching the filters.
     */
    list(options?: LibraryGameQuery): Promise<Readonly<{ games: LibraryGame[]; total: number }>>;
    platforms(): Promise<string[]>;
    get(rootBinaryId: string): Promise<LibraryGame | null>;
    delete(rootBinaryId: string): Promise<void>;
  }>;
  readonly launchables: Readonly<{
    add(input: { path: string; platform?: string; parentBinaryId?: string; artifact?: { storage: string; id: string; entrypoint?: string; package?: string } }): Promise<Readonly<{ launchable: LibraryLaunchable; job: Job | null; imports?: readonly GameImport[] }>>;
    find(uri: string): Promise<LibraryLaunchable | null>;
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

export interface MachineFirmware {
  names: string[];
  md5: string | null;
  sha1: string | null;
  sha256: string | null;
}

export interface MachineSpec {
  id: string;
  name: string;
  kind: "console" | "handheld" | "addon" | "arcade";
  shortName: string | null;
  modelNumbers: string[];
  licensor: string | null;
  manufacturer: string | null;
  dependencies: string[];
  regions: {
    region: "northAmerica" | "europe" | "japan";
    name: string | null;
    englishName: string | null;
    shortName: string | null;
    modelNumbers: string[];
    releaseDate: string | null;
  }[];
  inputGroups: { name: string; inputs: string[]; ports: number; isAccessoryGroup: boolean }[];
  accessories: string[];
  storageDevices: {
    id: string;
    name: string;
    isRaw: boolean;
    isUserStorage: boolean;
    partitions: { id: string; name: string; isUserData: boolean }[];
  }[];
  bios: MachineFirmware[];
  sysupdates: MachineFirmware[];
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
      /** UI-only host metadata; unavailable in plugin isolates. */
      readonly meta: Readonly<{ debug: boolean }>;
      readonly scraping: ScrapingApi;
      readonly configuration: ConfigurationApi;
      readonly consolestorage: ConsoleStorageApi;
      readonly emulator: EmulatorApi;
      readonly plugin: PluginApi;
      readonly webrtc: WebRtcApi;
      readonly input: InputApi;
      readonly controllers: ControllersApi;
      readonly library: LibraryApi;
      readonly jobs: JobsApi;
      machine(id: string): MachineSpec | null;
    }>;
  }

  interface HTMLElementTagNameMap {
    "tracked-surface": HTMLTrackedSurfaceElement;
  }
}
