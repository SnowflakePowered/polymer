import config from "pkg:plugin.toml";

type Configuration = {
    options(): {
        ui: {
            settings_version: number,
            setup_wizard_incomplete: boolean,
        },
        folders: {
            bios: string,
            snapshots: string,
            savestates: string,
            memory_cards: string,
            logs: string,
            cheats: string,
            patches: string,
            user_resources: string,
            cache: string,
            textures: string,
            input_profiles: string,
            videos: string,
            debugger_layouts: string,
            debugger_settings: string,
        },
        filenames: { bios: string },
        memory_cards: {
            slot1_enable: boolean,
            slot1_filename: string,
            slot2_enable: boolean,
            slot2_filename: string,
        },
        input_sources: { sdl: boolean },
        "emucore-gs": { renderer: number },
    },
    render(bindings: readonly GamepadBinding[]): Readonly<Record<string, Uint8Array>>,
};

type GamepadBinding = Readonly<{
    target: Readonly<{ group: string, port: number }>,
    gamepad: number,
}>;

type Resource = Readonly<{ kind: "tree" | "file", path?: string }>;

type Filesystem = {
    mountHostFile(target: string, source: string, options: { mode: string }): void,
    mountTree(target: string, source: Resource | URL | string, options: { mode: string }): void,
    mountFile(target: string, source: Resource | URL | string | Uint8Array, options: { mode: string }): void,
    mountStorage(target: string, source: Resource, options: { mode: string, format: string }): void,
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

type SetupLaunchArgs = {
    launchPath: string,
    console: string,
    portAssignments: { [group: string]: string[] },
    gamepadBindings: readonly GamepadBinding[],
    configuration: Configuration,
    filesystem: Filesystem,
    resources: { runtime: Resource },
    storage: Readonly<{ mem1?: Resource, mem2?: Resource }>,
};

function basename(path: string): string {
    const name = path.replaceAll("\\", "/").split("/").at(-1);
    if (!name) throw new TypeError(`path has no filename: ${path}`);
    return name;
}

export const setupLaunch = (args: SetupLaunchArgs) => {
    const biosName = basename(config.plugin.config.bios);
    const gameName = basename(args.launchPath);

    // Keep the immutable PCSX2 distribution separate from invocation-local
    // state. Portable mode only needs its fixed `inis/` child to be writable;
    // every configurable data directory is redirected to /User below.
    args.filesystem.mountTree("/PCSX2", args.resources.runtime, { mode: "rx" });
    args.filesystem.mountScratch("/PCSX2/inis", { mode: "rwx", backing: "memory" });
    args.filesystem.mountScratch("/User", { mode: "rwx", backing: "temporary" });
    if (args.storage.mem1) {
        args.filesystem.mountStorage("/User/memcards/Mcd001.ps2", args.storage.mem1, {
            mode: "rwx",
            format: config.plugin.config.memory_card_format,
        });
    }
    args.filesystem.mountHostFile(`/User/bios/${biosName}`, config.plugin.config.bios, { mode: "r" });
    args.filesystem.mountHostFile(`/Game/${gameName}`, args.launchPath, { mode: "r" });
    args.filesystem.setWorkingDirectory("/PCSX2");
    args.filesystem.setProfile({
        home: "/User",
        roamingAppData: "/User/AppData/Roaming",
        localAppData: "/User/AppData/Local",
        temp: "/User/Temp",
    });

    const options = args.configuration.options();
    options.ui.settings_version = 1;
    options.ui.setup_wizard_incomplete = false;
    options.folders.bios = args.filesystem.path("/User/bios");
    options.folders.snapshots = args.filesystem.path("/User/snaps");
    options.folders.savestates = args.filesystem.path("/User/sstates");
    options.folders.memory_cards = args.filesystem.path("/User/memcards");
    options.folders.logs = args.filesystem.path("/User/logs");
    options.folders.cheats = args.filesystem.path("/User/cheats");
    options.folders.patches = args.filesystem.path("/User/patches");
    options.folders.user_resources = args.filesystem.path("/User/resources");
    options.folders.cache = args.filesystem.path("/User/cache");
    options.folders.textures = args.filesystem.path("/User/textures");
    options.folders.input_profiles = args.filesystem.path("/User/inputprofiles");
    options.folders.videos = args.filesystem.path("/User/videos");
    options.folders.debugger_layouts = args.filesystem.path("/User/debuggerlayouts");
    options.folders.debugger_settings = args.filesystem.path("/User/debuggersettings");
    options.filenames.bios = biosName;
    options.memory_cards.slot1_enable = true;
    options.memory_cards.slot1_filename = "Mcd001.ps2";
    options.memory_cards.slot2_enable = false;
    options.memory_cards.slot2_filename = "Mcd002.ps2";
    options.input_sources.sdl = true;
    options["emucore-gs"].renderer = 3;
    for (const [name, bytes] of Object.entries(args.configuration.render(args.gamepadBindings))) {
        args.filesystem.mountFile(`/PCSX2/${name}`, bytes, { mode: "rw" });
    }

    return {
        command: args.filesystem.path("/PCSX2/pcsx2-qt.exe"),
        argv: ["-portable", "-batch", "--", args.filesystem.path(`/Game/${gameName}`)],
        isolate: false,
        hooks: ["d3d11"],
        surfaces: [
            { index: 0 },
        ],
    };
};

type ControllerChangeContext = Readonly<{
    controllerMappings: readonly Readonly<{
        target: Readonly<{ group: string, port: number }>,
        gamepad: number,
        layout: string,
        connected: boolean,
    }>[],
    gamepads: readonly Readonly<{
        index: number,
        connected: boolean,
        connect(): void,
        disconnect(): void,
    }>[],
    configuration: Configuration,
    filesystem: Filesystem,
    flush(): void,
}>;

export const onControllerChanged = (context: ControllerChangeContext) => {
    const bindings = context.controllerMappings.map(mapping => ({
        target: mapping.target,
        gamepad: context.gamepads[mapping.gamepad],
    }));
    for (const mapping of context.controllerMappings) {
        const gamepad = context.gamepads[mapping.gamepad];
        if (!gamepad) throw new TypeError(`virtual gamepad ${mapping.gamepad} is unavailable`);
        if (mapping.connected) gamepad.connect();
        else gamepad.disconnect();
    }
    for (const [name, bytes] of Object.entries(context.configuration.render(bindings))) {
        context.filesystem.mountFile(`/PCSX2/${name}`, bytes, { mode: "rw" });
    }
};

export default { setupLaunch, onControllerChanged };
