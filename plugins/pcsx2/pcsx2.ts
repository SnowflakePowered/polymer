import config from "pkg:plugin.toml";
import type {
    Actions,
    Configuration as HostConfiguration,
    ControllerChangeContext as HostControllerChangeContext,
    Filesystem,
    Resource,
    PauseResumeContext,
} from "../plugin-api.ts";
import { HotkeyParser } from "./hotkeys.ts";

type Configuration = HostConfiguration<{
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
}>;

type EmulatorLaunchArgs = {
    actions: Actions,
    launchPath: string,
    console: string,
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

export const onLaunch = (args: EmulatorLaunchArgs) => {
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
    for (const [name, bytes] of Object.entries(args.configuration.render())) {
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

type ControllerChangeContext = HostControllerChangeContext<ReturnType<Configuration["options"]>>;

export const onControllerChanged = (context: ControllerChangeContext) => {
    for (const [name, bytes] of Object.entries(context.configuration.render())) {
        context.filesystem.mountFile(`/PCSX2/${name}`, bytes, { mode: "rw" });
    }
};

export const onParseHotkey = (expression: string) => HotkeyParser.pcsx2(expression);

export const onPauseResume = (context: PauseResumeContext) => {
    context.actions.pause?.activate();
};

export default { onLaunch, onControllerChanged, onParseHotkey, onPauseResume };
