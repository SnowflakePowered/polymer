/** Shared host-owned values exposed to emulator plugins. */
export type Resource = Readonly<{
    kind: "tree" | "file",
    path?: string,
}>;

export type Filesystem = {
    mountCache(cacheKey: string): Resource,
    mountHostTree(target: string, source: string, options: { mode: string }): void,
    mountHostFile(target: string, source: string, options: { mode: string }): void,
    mountTree(target: string, source: Resource | URL | string, options: { mode: string }): void,
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

export type InputSourceSelector =
    | Readonly<{ kind: "keyboardMouse" }>
    | Readonly<{ kind: "device", deviceId: string }>;

export type ControllerMapping = Readonly<{
    target: Readonly<{ group: string, port: number }>,
    gamepad: number,
    source: InputSourceSelector | null,
    layout: string,
    connected: boolean,
    device?: Readonly<{
        id: string,
        source: "sdl" | "bt",
        name: string,
        connected: boolean,
        metadata: Readonly<Record<string, unknown>>,
    }>,
    mouse: boolean,
    projectionMode: "name" | "position" | "custom",
    mapping: Readonly<Record<string, string>>,
}>;

export type ControllerTopology = Readonly<{
    mappings: readonly ControllerMapping[],
}>;

export type InputHook = "bluetooth-api-win32";
export type InputProvider = Readonly<{
    type: "bluetoothHidPipe",
    sourceId: string,
    port: number,
    productId: number,
}>;

export type UiTextMatch =
    | Readonly<{ equals: string, caseSensitive?: boolean }>
    | Readonly<{ contains: string, caseSensitive?: boolean }>;

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
        query: { title?: UiTextMatch, className?: UiTextMatch, visible?: boolean },
        options?: { timeoutMs?: number },
    ): Promise<UiWindow>,
};

export type ControllerChangeContext<Options = unknown> = Readonly<{
    topology: ControllerTopology,
    previousTopology: ControllerTopology,
    configuration: Configuration<Options>,
    filesystem: Filesystem,
    ui: UiAutomation,
}>;
