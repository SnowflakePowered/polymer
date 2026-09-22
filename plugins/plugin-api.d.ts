declare module "polymer:sys" {
    /** Operating system and architecture of the Polymer host. */
    export const osIdentifier: string;
}

/** Host API types. Shared configuration references are declaration-only. */
declare module "polymer:api" {
    export type DigestAlgorithm = import("../js/package.ts").DigestAlgorithm;
    export type Digestible = import("../js/package.ts").Digestible;
    /** Shared host-owned values exposed to emulator plugins. */
    export type Storage = Readonly<{
        kind: "storage",
    }>;

    /** Host-provided storage for a ROM-adjacent battery save file. */
    export type BatteryStorage = Readonly<{ battery?: Storage }>;

    export type DirectoryEntry = Readonly<{
        name: string,
        kind: "directory",
        [Symbol.asyncIterator](): AsyncIterator<DirEntry>,
        remove(name: string): Promise<void>,
    }>;

    export type FileEntry = Digestible & Readonly<{
        name: string,
        kind: "file",
        bytes(): Promise<Uint8Array>,
        text(): Promise<string>,
    }>;

    export type DirEntry = DirectoryEntry | FileEntry;
    export type DirectoryResource = DirectoryEntry;

    export type PluginStorage = Readonly<{
        getItem<T = unknown>(key: string): T | null,
        setItem(key: string, value: unknown): void,
    }>;

    export type PluginProgressContext = Readonly<{
        progress(update: { message: string; completed?: number; total?: number }): void;
    }>;
    export type PluginLoadContext = PluginProgressContext;
    export type UpdateContents = (context: PluginProgressContext) => void | Promise<void>;
    export type OnPluginLoad = (context: PluginLoadContext) => void | Promise<void>;

    export type PluginCacheFile = Digestible & Readonly<{
        readonly size: number,
        readable?: ReadableStream<Uint8Array>,
        writable?: WritableStream<Uint8Array | string>,
        read(offset: number, length: number): Promise<Uint8Array>,
        bytes(): Promise<Uint8Array>,
        text(): Promise<string>,
    }>;
    export type PluginCache = DirectoryEntry & Readonly<{
        open(path: string, options?: { read?: boolean, write?: boolean, create?: boolean, truncate?: boolean }): PluginCacheFile,
    }>;

    export type PluginStatement = Readonly<{
        run(values?: readonly unknown[]): Promise<{ changes: number, lastInsertRowid: number }>,
        get<T = Record<string, unknown>>(values?: readonly unknown[]): Promise<T | undefined>,
        all<T = Record<string, unknown>>(values?: readonly unknown[]): Promise<T[]>,
        iterate<T = Record<string, unknown>>(values?: readonly unknown[]): AsyncIterable<T>,
    }>;
    export type PluginDatabase = Readonly<{
        prepare(sql: string): PluginStatement,
        exec(sql: string): Promise<void>,
        batch(items: readonly (readonly [string, (readonly unknown[])?] | { sql: string, args?: readonly unknown[] })[], mode?: "read" | "write"): Promise<unknown[]>,
        transactionAsync<T>(callback: (database: PluginDatabase) => Promise<T>, mode?: "read" | "write"): Promise<T>,
        close(): Promise<void>,
    }>;

    export type ScrapeScalar = string | number | boolean;
    export type ScrapeCandidate = Readonly<{ source: string, value: ScrapeScalar }>;
    export type ScrapeInput = Readonly<{
        [key: `${string}.${string}`]: readonly ScrapeCandidate[];
        /** Raw dump files remain associated with their package, including CUE backing files. */
        packages: readonly (Package & Readonly<{ files: readonly (Pick<FileEntry, "name" | "kind" | "digest"> & { readonly size: number })[] }>)[];
    }>;
    /** Raw media bytes; use application/x-cue for media.cue. */
    export type ScrapeInlineMedia = Readonly<{
        kind: "inline", mediaType: string, bytes: readonly number[],
    }>;
    export type ScrapeResult = Readonly<{ key: string, value: ScrapeScalar | ScrapeInlineMedia, source: string }>;
    export type OnScrape = (input: ScrapeInput) => AsyncGenerator<ScrapeResult, void, void>;

    export type Mountable = DirEntry | Uint8Array | Configuration | Storage | Package;

    export type Filesystem = {
        /** Mounts a temporary scratch directory; mode defaults to rwx. */
        mount(target: string, options?: { mode?: string, format?: never }): string,
        /** Returns the emulator-visible mounted path, or the entrypoint inside a package. Missing parents outside explicit mounts become rwx scratch directories when the plan is finalized. Configurations refresh on flush and controller changes. */
        mount(target: string, source: Mountable, options: { mode: string, format?: never }): string,
        /** Memory-card storage defaults to the raw format when format is omitted. */
        mount(target: string, source: Storage, options: { mode: string, format?: string }): string,
        /** Publishes staged mounted-file and configuration changes to the running emulator. */
        flush(): void,
        path(path: string): string,
    };

    export type Configuration<Options = unknown> = {
        options(): Options,
        render(): Readonly<Record<string, Uint8Array>>,
    };

    export type RepeatedSection<Options> = Readonly<{
        append(): Options,
    }>;

    /** digest() hashes the whole file; binary.digest() hashes the normalized executable. */
    export type Package = import("../js/package.ts").Package & Digestible & Readonly<{ binary: Digestible }>;

    export type ConfigurationChoice = import("../js/configuration.ts").ConfigurationChoice;
    export type UserConfiguration = import("../js/configuration.ts").UserConfiguration;
    type SharedConfigurationGroup<T> = import("../js/configuration.ts").ConfigurationGroup<T>;
    type SharedConfigurationLayout<T> = import("../js/configuration.ts").ConfigurationLayout<T>;
    type ExpandedConfigurationLayoutOption = import("../js/configuration.ts").ConfigurationLayoutOption;

    export type ConfigurationLayoutOption =
        | Readonly<{ key: string, source: string }>
        | ExpandedConfigurationLayoutOption;
    export type ConfigurationGroup = SharedConfigurationGroup<ConfigurationLayoutOption>;
    export type ConfigurationLayout = SharedConfigurationLayout<ConfigurationLayoutOption>;
    export type ConfigurationResult = ConfigurationLayout & Readonly<{
        values: UserConfiguration,
    }>;

    export type OnConfigure = (
        game: Package,
        userConfiguration?: UserConfiguration,
    ) => ConfigurationResult | Promise<ConfigurationResult>;

    export type InputSourceSelector =
        | Readonly<{ kind: "keyboardMouse" }>
        | Readonly<{ kind: "device", deviceId: string }>;

    export type ControllerDevice = Readonly<{
        id: string,
        name: string,
        profile: string | null,
        vendorId: number | null,
        productId: number | null,
        serial: string | null,
        role: "Whole" | "LeftHalf" | "RightHalf",
        capabilities: readonly string[],
    }>;

    export type ControllerMapping = Readonly<{
        target: Readonly<{ group: string, port: number }>,
        gamepad: number,
        source: InputSourceSelector | null,
        layout: string,
        connected: boolean,
        device?: ControllerDevice,
        mouse: boolean,
        projectionMode: "name" | "position" | "custom",
        mapping: Readonly<Record<string, string>>,
        pointer: Readonly<{
            source: "none" | "deviceNative" | "hostMouse" | "deviceThenHostMouse",
            mode: "none" | "relative" | "absoluteDevice" | "absoluteScreen",
        }>,
    }>; 

    export type ControllerTopology = Readonly<{
        mappings: readonly ControllerMapping[],
    }>;

    export type InputHook = "sdl3" | "bluetooth-api-win32";

    export type UiNodeSnapshot = Readonly<{
        role: string,
        name: string | null,
        value: string | null,
        stableId: string | null,
        states: Readonly<Record<string, unknown>>,
    }>;

    export type UiLocator = {
        nth(index: number): UiLocator,
        all(options?: { limit?: number }): Promise<readonly UiNodeSnapshot[]>,
        snapshot(): Promise<UiNodeSnapshot>,
        click(options?: { button?: string, count?: number }): Promise<void>,
        press(): Promise<void>,
        select(): Promise<void>,
        toggle(): Promise<void>,
        selectOption(name: string): Promise<void>,
    };

    export type UiWindow = {
        readonly id: number,
        readonly keyboard: {
            press(
                code: string,
                options?: { modifiers?: readonly string[], repeat?: boolean },
            ): Promise<void>,
        },
        locator(selector: string): UiLocator,
        setLogicalFocus(): Promise<void>,
    };

    export type UiAutomation = {
        windows(query?: {
            /** Rust-regex pattern matched against the window title. */
            pattern?: string,
            /** Accessibility selector that must match within the window. */
            selector?: string,
        }): Promise<readonly UiWindow[]>,
        waitForWindow(
            query: {
                /** Rust-regex pattern matched against the window title. */
                pattern?: string,
                /** Accessibility selector that must match within the window. */
                selector?: string,
            },
            options?: { timeoutMs?: number },
        ): Promise<UiWindow>,
    };

    export type ParsedHotkey =
        | Readonly<{ kind: "keyboard", code: string, modifiers?: readonly string[] }>
        | Readonly<{ kind: "binding", player: number, keys: readonly string[] }>;

    export type HotkeyAction = Readonly<{
        activate(): void,
    }>;

    export type Actions = Readonly<Record<string, HotkeyAction>>;

    export interface ActionsContext {
        readonly actions: Actions;
    }

    export type PauseResumeContext = ActionsContext & Readonly<{
        pauseState: "pause" | "resume",
    }>;

    export type EmulatorLaunchable = Readonly<{
        launchPath: string,
        package: Package,
    }>;

    /** Ordered discs supplied to onLaunch; mount their files before starting the emulator. */
    export type EmulatorLaunchSet = Readonly<{
        launchSet: readonly EmulatorLaunchable[],
        initialDiscIndex: number,
    }>;

    export type DiscChangeContext = ActionsContext & Readonly<{
        launchSet: readonly EmulatorLaunchable[],
        index: number,
        filesystem: Filesystem,
    }>;

    export type OnDiscChange = (context: DiscChangeContext) => void | Promise<void>;

    export type StateContext = ActionsContext & Readonly<{
        slot: number,
        filesystem: Filesystem,
        directory: DirectoryResource,
    }>;

    export type QuitContext = ActionsContext & Readonly<{
        filesystem: Filesystem,
        directory: DirectoryResource,
        launchPath: string,
        console: string,
        package: Package,
        userConfiguration?: UserConfiguration,
    }>;

    export type ControllerChangeContext<Options = unknown> = ActionsContext & Readonly<{
        topology: ControllerTopology,
        previousTopology: ControllerTopology,
        configuration: Configuration<Options>,
        filesystem: Filesystem,
    }>;
}


declare module "polymer:api" {
    export type DepotType = "system" | "app" | "appcontent" | "appupdate";
    export interface PlatformArtifact { id: string; label: string; version: string | null; depotType: DepotType; byteLength: number }
    export interface BootstrapDevice {
        readonly artifacts: readonly PlatformArtifact[];
        readonly id: string;
        readonly platform: string;
        install(source: string | PluginCacheFile): Promise<PlatformArtifact>;
    }
    export interface BootstrapContext extends PluginProgressContext, FirmwareContext, KeyWriterContext { readonly settings: Record<string, unknown> }
}

declare module "polymer:consolestorage" {
    /** Read-only inspection of configured disk paths or pkg: resources. */
    export const resources: { inspect(path: string): { exists: boolean; size?: number; directory?: boolean } };
}

/** User-supplied BIOS files recognized by consolespec, separate from system software and keys. */
declare module "polymer:api" {
    export type FirmwareLookup = { readonly name: string } | {
        readonly md5?: string;
        readonly sha1?: string;
        readonly sha256?: string;
    };
    export interface FirmwareDescription {
        readonly platform: string;
        readonly label: string;
        readonly names: readonly string[];
        readonly byteLength: number;
    }
    /** Mixed into onLaunch arguments and the onBootstrap context by the host. */
    export interface FirmwareContext { readonly firmwareStore: FirmwareStore }
    export interface FirmwareStore {
        list(): Promise<readonly FirmwareDescription[]>;
        /** Strings delegate to { name }. Hash objects use their first hash in property order. */
        open(name: string): Promise<FileEntry>;
        open(lookup: FirmwareLookup): Promise<FileEntry>;
    }

    export interface KeyIdentity { readonly namespace: "system" | "content"; readonly id: string }
    export interface KeyInput extends KeyIdentity { readonly titleId?: string; readonly bytes: Uint8Array }
    export interface KeyDescription extends KeyIdentity { readonly titleId?: string; readonly byteLength: number }
    export interface KeyQuery { readonly namespace?: "system" | "content"; readonly titleId?: string }
    export interface KeyReader {
        list(query?: KeyQuery): Promise<readonly KeyDescription[]>;
        open(identity: KeyIdentity): Promise<FileEntry>;
    }
    export interface KeyWriter extends KeyReader {
        put(records: readonly KeyInput[]): Promise<void>;
        remove(identity: KeyIdentity): Promise<boolean>;
    }
    export interface KeyReaderContext { readonly keyStore: KeyReader }
    export interface KeyWriterContext { readonly keyStore: KeyWriter }
}
