import { caches, config } from "polymer:plugin";
import { connect } from "polymer:database";
import { download } from "polymer:download";
import { Job } from "polymer:jobs";
import { graphicsApis, osIdentifier } from "polymer:sys";
import { unarchive } from "polymer:unarchive";
import { EmulatorUpdates } from "jsr:@polymer-fe/emulator-updates@^0.1.0";
import type { ConfigurationResult, EmulatorLaunchArgs, Package, PauseResumeContext, Storage, GraphicsHook, UserConfiguration } from "polymer:api";
import { HotkeyParser } from "pkg:hotkeys.ts";

type Options = {
        ui: {
            settings_version: number,
            setup_wizard_incomplete: boolean,
            render_to_separate_window: boolean,
            hide_main_window_when_running: boolean,
            double_click_toggles_fullscreen: boolean,
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
        "emucore-gs": { renderer: number, upscale_multiplier: string },
        "emucore-speedhacks": { ee_cycle_rate: number },
};

const runtime = new EmulatorUpdates({
    name: "PCSX2",
    cache: caches.runtime,
    osIdentifier,
    channel: String(config.channel ?? "stable"),
    connect,
    download,
    unarchive,
    Job,
});

export const onPluginLoad = () => runtime.ready();

export const onUpdateContents = async () => runtime.update([await runtime.release("pcsx2")]);

export const onConfigure = (_pkg: Package, userConfiguration?: UserConfiguration): ConfigurationResult => ({
    schema: "pcsx2/v1",
    groups: [{
        key: "graphics",
        title: "Graphics",
        options: [...(GRAPHICS_APIS.length > 1 ? [GRAPHICS_API_OPTION] : []), {
            key: "resolution",
            title: "Internal Resolution",
            description: "Multiple of the console's native resolution.",
            type: "int",
            default: 1,
            min: 1,
            max: MAX_RESOLUTION,
        }],
    }, {
        key: "cpu",
        title: "CPU",
        options: [CPU_CLOCK_OPTION],
    }],
    values: { graphics: { api: graphicsApi(userConfiguration)?.hook ?? null, resolution: internalResolution(userConfiguration) }, cpu: { clock: cpuClock(userConfiguration).key } },
});

const GRAPHICS_APIS = ([
    { hook: "d3d11", label: "Direct3D 11", value: 3 },
    { hook: "d3d12", label: "Direct3D 12", value: 15 },
    { hook: "metal", label: "Metal", value: 17 },
    { hook: "vulkan", label: "Vulkan", value: 14 },
    { hook: "opengl", label: "OpenGL", value: 12 },
] satisfies { hook: GraphicsHook, label: string, value: number }[]).filter(api => graphicsApis.includes(api.hook));

const GRAPHICS_API_OPTION = {
    key: "api",
    title: "Graphics API",
    type: "select",
    default: GRAPHICS_APIS[0]?.hook ?? null,
    choices: GRAPHICS_APIS.map(api => ({ key: api.hook, label: api.label })),
} as const;

function graphicsApi(configuration?: UserConfiguration) {
    return GRAPHICS_APIS.find(api => api.hook === configuration?.graphics?.api) ?? GRAPHICS_APIS[0];
}

const CPU_CLOCKS = [
    { key: "100", label: "100% (Default)", value: 0 },
    { key: "130", label: "130%", value: 1 },
    { key: "180", label: "180%", value: 2 },
    { key: "300", label: "300%", value: 3 },
];

const CPU_CLOCK_OPTION = {
    key: "clock",
    title: "CPU Clock",
    description: "Percentage of the console's native CPU speed. Higher values can reduce slowdown but may break some games.",
    type: "select",
    default: "100",
    choices: CPU_CLOCKS.map(clock => ({ key: clock.key, label: clock.label })),
} as const;

function cpuClock(configuration?: UserConfiguration) {
    return CPU_CLOCKS.find(clock => clock.key === configuration?.cpu?.clock) ?? CPU_CLOCKS[0];
}

const MAX_RESOLUTION = 8;

function internalResolution(configuration?: UserConfiguration): number {
    const value = configuration?.graphics?.resolution;
    return typeof value === "number" && Number.isInteger(value) ? Math.min(MAX_RESOLUTION, Math.max(1, value)) : 1;
}

export const onLaunch = async (args: EmulatorLaunchArgs<Options, Readonly<{ mem1?: Storage, mem2?: Storage }>>) => {
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
    args.filesystem.mount("/PCSX2", await runtime.launch(), { mode: "rx" });
    args.filesystem.mount("/PCSX2/inis");
    if (args.storage.mem1) {
        args.filesystem.mount("/User/memcards/Mcd001.ps2", args.storage.mem1, {
            mode: "rwx",
            format: String(config.memory_card_format),
        });
    }
    args.filesystem.mount(`/User/bios/${biosName}`, biosFile, { mode: "r" });
    const gamePath = args.filesystem.mount("/Game", args.launchSet[0].package, { mode: "rx" });

    const options = args.configuration.options();
    options.ui.settings_version = 1;
    options.ui.setup_wizard_incomplete = false;
    // Keep the game surface at PCSX2's default 4:3 window size while leaving
    // the main window and its menus available for UI automation.
    options.ui.render_to_separate_window = true;
    options.ui.hide_main_window_when_running = false;
    options.ui.double_click_toggles_fullscreen = false;
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
    options["emucore-gs"].renderer = graphicsApi(args.userConfiguration).value;
    options["emucore-speedhacks"].ee_cycle_rate = cpuClock(args.userConfiguration).value;
    options["emucore-gs"].upscale_multiplier = String(internalResolution(args.userConfiguration));
    args.filesystem.mount("/PCSX2", args.configuration, { mode: "rw" });

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
        hooks: { graphics: [graphicsApi(args.userConfiguration).hook], input: ["sdl3"] },
        
        surfaces: [
            { index: 0 },
        ],
    };
};

export const onParseHotkey = (expression: string) => HotkeyParser.parse(expression);

export const onPauseResume = (context: PauseResumeContext) => {
    context.actions.pause?.activate();
};

export default { onPluginLoad, onUpdateContents, onConfigure, onLaunch, onParseHotkey, onPauseResume };
