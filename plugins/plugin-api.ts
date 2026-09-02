/** Shared host-owned values exposed to emulator plugins. */
export type Resource = Readonly<{
    kind: "tree" | "directory" | "file",
    path?: string,
}>;

export type DirectoryEntry = Readonly<{
    name: string,
    kind: "directory",
    [Symbol.asyncIterator](): AsyncIterator<DirEntry>,
}>;

export type FileEntry = Readonly<{
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

export type OnPluginLoad = () => void | Promise<void>;

export type Filesystem = {
    mountCache(cacheKey: string): Resource,
    mountHostTree(target: string, source: string, options: { mode: string }): void,
    mountHostFile(target: string, source: string, options: { mode: string }): void,
    mountTree(target: string, source: Resource | DirectoryResource | URL | string, options: { mode: string }): void,
    mountFile(target: string, source: Resource | URL | string | Uint8Array, options: { mode: string }): void,
    mountConfiguration(target: string, configuration: Configuration, options: { mode: string }): void,
    mountStorage(target: string, source: Resource, options: { mode: string, format?: string }): void,
    mountScratch(target: string, options: { mode: string, backing: "memory" | "temporary" }): void,
    path(path: string): string,
    setWorkingDirectory(path: string): void,
    setProfile(profile: {
        home?: string,
        roamingAppData?: string,
        localAppData?: string,
        temp?: string,
    }): void,
};

export type Configuration<Options = unknown> = {
    options(): Options,
    render(): Readonly<Record<string, Uint8Array>>,
};

export type RepeatedSection<Options> = Readonly<{
    append(): Options,
}>;

export type ConfigureGame = Readonly<{
    launchPath: string,
    name: string,
    platform: string,
    serial?: string,
    titleId?: string,
}>;

export type {
    ConfigurationChoice,
    UserConfiguration,
} from "../js/configuration.ts";
import type {
    ConfigurationGroup as SharedConfigurationGroup,
    ConfigurationLayout as SharedConfigurationLayout,
    ConfigurationLayoutOption as ExpandedConfigurationLayoutOption,
    UserConfiguration,
} from "../js/configuration.ts";

export type ConfigurationLayoutOption =
    | Readonly<{ key: string, source: string }>
    | ExpandedConfigurationLayoutOption;
export type ConfigurationGroup = SharedConfigurationGroup<ConfigurationLayoutOption>;
export type ConfigurationLayout = SharedConfigurationLayout<ConfigurationLayoutOption>;
export type ConfigurationResult = ConfigurationLayout & Readonly<{
    values: UserConfiguration,
}>;

export type OnConfigure = (
    game: ConfigureGame,
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

export type UiLocator = {
    nth(index: number): UiLocator,
    press(): Promise<void>,
    selectOption(name: string): Promise<void>,
};

export type UiWindow = {
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

export type PauseResumeContext = Readonly<{
    pauseState: "pause" | "resume",
    actions: Actions,
    ui: UiAutomation,
}>;

export type ControllerChangeContext<Options = unknown> = Readonly<{
    topology: ControllerTopology,
    previousTopology: ControllerTopology,
    configuration: Configuration<Options>,
    filesystem: Filesystem,
    ui: UiAutomation,
    actions: Actions,
}>;
