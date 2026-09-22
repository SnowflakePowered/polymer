import runtime from "pkg:bin/win-x64";
import config from "pkg:plugin.toml";
import type {
    Package,
    Configuration as HostConfiguration,
    ControllerChangeContext as HostControllerChangeContext,
    Filesystem,
    FirmwareContext,
    Storage,
    PauseResumeContext,
} from "polymer:api";
import { HotkeyParser } from "pkg:hotkeys.ts";

type Configuration = HostConfiguration<{
        ui: {
            settings_version: number,
            setup_wizard_incomplete: boolean,
            render_to_separate_window: boolean,
            hide_main_window_when_running: boolean,
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

type EmulatorLaunchArgs = FirmwareContext & {
    launchPath: string,
    package: Package,
    console: string,
    configuration: Configuration,
    filesystem: Filesystem,
    storage: Readonly<{ mem1?: Storage, mem2?: Storage }>,
};

export const onLaunch = async (args: EmulatorLaunchArgs) => {
    const bios = (await args.firmwareStore.list()).find(entry =>
        entry.platform === "SONY_PS2" && entry.byteLength === 4 * 1024 * 1024
    );
    if (!bios) {
        throw new Error("PCSX2 requires a PS2 BIOS. Drag and drop a PS2 BIOS file into Polymer, then launch again.");
    }
    const biosFile = await args.firmwareStore.open(bios.label);
    const biosName = "bios.bin";

    // Keep the immutable PCSX2 distribution separate from invocation-local
    // state. Portable mode only needs its fixed `inis/` child to be writable;
    // every configurable data directory is redirected to /User below.
    args.filesystem.mount("/PCSX2", runtime, { mode: "rx" });
    args.filesystem.mount("/PCSX2/inis");
    if (args.storage.mem1) {
        args.filesystem.mount("/User/memcards/Mcd001.ps2", args.storage.mem1, {
            mode: "rwx",
            format: config.plugin.config.memory_card_format,
        });
    }
    args.filesystem.mount(`/User/bios/${biosName}`, biosFile, { mode: "r" });
    const gamePath = args.filesystem.mount("/Game", args.package, { mode: "rx" });

    const options = args.configuration.options();
    options.ui.settings_version = 1;
    options.ui.setup_wizard_incomplete = false;
    // Keep the game surface at PCSX2's default 4:3 window size while leaving
    // the main window and its menus available for UI automation.
    options.ui.render_to_separate_window = true;
    options.ui.hide_main_window_when_running = false;
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
        args.filesystem.mount(`/PCSX2/${name}`, bytes, { mode: "rw" });
    }

    return {
        command: args.filesystem.path("/PCSX2/pcsx2-qt.exe"),
        workingDirectory: args.filesystem.path("/PCSX2"),
        env: [
            ["USERPROFILE", args.filesystem.path("/User")],
            ["APPDATA", args.filesystem.path("/User/AppData/Roaming")],
            ["LOCALAPPDATA", args.filesystem.path("/User/AppData/Local")],
            ["TEMP", args.filesystem.path("/User/Temp")],
            ["TMP", args.filesystem.path("/User/Temp")],
        ],
        argv: ["-portable", "-batch", "--", gamePath],
        isolate: false,
        hooks: { graphics: ["d3d11"], input: ["sdl3"] },
        
        surfaces: [
            { index: 0 },
        ],
    };
};

type ControllerChangeContext = HostControllerChangeContext<ReturnType<Configuration["options"]>>;

export const onControllerChanged = (context: ControllerChangeContext) => {
    for (const [name, bytes] of Object.entries(context.configuration.render())) {
        context.filesystem.mount(`/PCSX2/${name}`, bytes, { mode: "rw" });
    }
};

export const onParseHotkey = (expression: string) => HotkeyParser.pcsx2(expression);

export const onPauseResume = (context: PauseResumeContext) => {
    context.actions.pause?.activate();
};

export default { onLaunch, onControllerChanged, onParseHotkey, onPauseResume };
