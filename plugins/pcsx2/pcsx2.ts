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
    },
};

type Resource = Readonly<{ kind: "tree" | "file", path?: string }>;

type Filesystem = {
    mountHostFile(target: string, source: string, options: { mode: string }): void,
    mountTree(target: string, source: Resource | URL | string, options: { mode: string }): void,
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
    configuration: Configuration,
    filesystem: Filesystem,
    resources: { runtime: Resource },
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
    args.filesystem.mountScratch("/PCSX2/inis", { mode: "rwx", backing: "temporary" });
    args.filesystem.mountScratch("/User", { mode: "rwx", backing: "temporary" });
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

    return {
        command: args.filesystem.path("/PCSX2/pcsx2-qt.exe"),
        argv: ["-portable", "-batch", "-fullscreen", "--", args.filesystem.path(`/Game/${gameName}`)],
        configRoot: args.filesystem.path("/PCSX2"),
        isolate: true,
        hooks: ["d3d12"],
        config: args.configuration,
        surfaces: [
            { index: 0, pattern: "PCSX2" },
        ],
    };
};

export default { setupLaunch };
