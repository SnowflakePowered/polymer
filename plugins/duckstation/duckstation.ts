import { caches, config } from "polymer:plugin";
import { connect } from "polymer:database";
import { download } from "polymer:download";
import { Job } from "polymer:jobs";
import { graphicsApis, osIdentifier } from "polymer:sys";
import { unarchive } from "polymer:unarchive";
import { EmulatorUpdates } from "jsr:@polymer-fe/emulator-updates@^0.1.0";
import type { ConfigurationResult, DiscChangeContext, EmulatorLaunchArgs, Package, PauseResumeContext, Storage, GraphicsHook, UserConfiguration } from "polymer:api";
import { HotkeyParser } from "pkg:hotkeys.ts";

type Options = {
        main: {
            settings_version: number,
            setup_wizard_incomplete: boolean,
            confirm_power_off: boolean,
            save_state_on_exit: boolean,
            pause_on_focus_loss: boolean,
            emulation_speed: number,
        },
        display: { vsync: boolean },
        bios: {
            search_directory: string,
            path_ntscu: string,
            patch_fast_boot: boolean,
        },
        folders: {
            cache: string,
            cheats: string,
            game_settings: string,
            input_profiles: string,
            patches: string,
            save_states: string,
            screenshots: string,
            textures: string,
            user_resources: string,
        },
        memorycards: {
            card1_type: string,
            card1_path: string,
            card2_type: string,
            directory: string,
            fast_forward_access: boolean,
        },
        logging: {
            log_level: string,
            log_to_console: boolean,
            log_to_file: boolean,
            memory_card: boolean,
            system: boolean,
        },
        input_sources: { sdl: boolean },
        gamelist: { recursive_paths: string },
        autoupdater: { check_at_startup: boolean },
        gpu: { resolution_scale: number, renderer: string },
        cpu: { overclock_enable: boolean, overclock_numerator: number, overclock_denominator: number },
};

let currentDisc = 0;

const runtime = new EmulatorUpdates({
    name: "DuckStation",
    cache: caches.runtime,
    osIdentifier,
    channel: String(config.channel ?? "stable"),
    connect,
    download,
    unarchive,
    Job,
});

export const onPluginLoad = () => runtime.ready();

export const onUpdateContents = async () => runtime.update([await runtime.release("duckstation")]);

export const onConfigure = (_pkg: Package, userConfiguration?: UserConfiguration): ConfigurationResult => ({
    schema: "duckstation/v1",
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
            choices: Array.from({ length: MAX_RESOLUTION }, (_, index) => ({ key: String(index + 1), label: `${320 * (index + 1)}×${240 * (index + 1)}` })),
        }, VSYNC_OPTION, FRAME_LIMIT_OPTION],
    }, {
        key: "cpu",
        title: "CPU",
        options: [CPU_CLOCK_OPTION],
    }],
    values: {
        graphics: {
            api: graphicsApi(userConfiguration)?.hook ?? null,
            resolution: internalResolution(userConfiguration),
            vsync: userConfiguration?.graphics?.vsync === true,
            frameLimit: frameLimit(userConfiguration),
        },
        cpu: { clock: cpuClock(userConfiguration) },
    },
});

const VSYNC_OPTION = {
    key: "vsync",
    title: "VSync",
    description: "Synchronize frames to the display's refresh rate to prevent tearing.",
    type: "bool",
    default: false,
} as const;

const FRAME_LIMIT_OPTION = {
    key: "frameLimit",
    title: "Frame Limiter",
    description: "Percentage of the console's native speed.",
    type: "int",
    default: 100,
    min: 50,
    max: 150,
} as const;

function frameLimit(configuration?: UserConfiguration): number {
    const value = configuration?.graphics?.frameLimit;
    return typeof value === "number" && Number.isInteger(value) ? Math.min(150, Math.max(50, value)) : 100;
}

const GRAPHICS_APIS = ([
    { hook: "d3d11", label: "Direct3D 11", value: "D3D11" },
    { hook: "d3d12", label: "Direct3D 12", value: "D3D12" },
    { hook: "metal", label: "Metal", value: "Metal" },
    { hook: "vulkan", label: "Vulkan", value: "Vulkan" },
    { hook: "opengl", label: "OpenGL", value: "OpenGL" },
] satisfies { hook: GraphicsHook, label: string, value: string }[]).filter(api => graphicsApis.includes(api.hook));

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

const MAX_CPU_CLOCK = 400;

const CPU_CLOCK_OPTION = {
    key: "clock",
    title: "CPU Clock",
    description: "Percentage of the console's native CPU speed. Higher values can reduce slowdown but may break some games.",
    type: "int",
    default: 100,
    min: 100,
    max: MAX_CPU_CLOCK,
} as const;

function cpuClock(configuration?: UserConfiguration): number {
    const value = configuration?.cpu?.clock;
    return typeof value === "number" && Number.isInteger(value) ? Math.min(MAX_CPU_CLOCK, Math.max(100, value)) : 100;
}

const MAX_RESOLUTION = 8;

function internalResolution(configuration?: UserConfiguration): number {
    const value = configuration?.graphics?.resolution;
    return typeof value === "number" && Number.isInteger(value) ? Math.min(MAX_RESOLUTION, Math.max(1, value)) : 1;
}

export const onLaunch = async (args: EmulatorLaunchArgs<Options, Readonly<{ card1?: Storage, card2?: Storage }>>) => {
    const biosFiles = (await args.firmwareStore.list()).filter(entry => entry.platform === "SONY_PSX");
    if (biosFiles.length === 0) {
        throw new Error("DuckStation requires a PlayStation BIOS. Drag and drop a PlayStation BIOS file into Polymer, then launch again.");
    }
    const path = (virtualPath: string) => args.filesystem.path(virtualPath);

    args.filesystem.mount("/DuckStation", await runtime.launch(), { mode: "rx" });
    args.filesystem.mount("/DuckStation/portable.txt", new Uint8Array(), { mode: "r" });
    args.filesystem.mount("/DuckStation/duckstation.log", new Uint8Array(), { mode: "rw" });
    if (args.storage.card1) {
        args.filesystem.mount("/User/memcards/polymer.mcd", args.storage.card1, {
            mode: "rw",
            format: "raw",
        });
    } else {
        args.filesystem.mount("/User/memcards");
    }
    for (const [index, bios] of biosFiles.entries()) {
        const file = await args.firmwareStore.open(bios.label);
        args.filesystem.mount(`/User/bios/bios-${index}.bin`, file, { mode: "r" });
    }
    const discPaths = args.launchSet.length > 1
        ? args.launchSet.map((disc, index) => args.filesystem.mount(`/Game/Disc${index}`, disc.package, { mode: "rx" }))
        : [args.filesystem.mount("/Game", args.launchSet[0].package, { mode: "rx" })];
    currentDisc = args.initialDiscIndex;

    const options = args.configuration.options();
    options.main.settings_version = 3;
    options.gpu.resolution_scale = internalResolution(args.userConfiguration);
    options.gpu.renderer = graphicsApi(args.userConfiguration).value;
    options.cpu.overclock_enable = cpuClock(args.userConfiguration) !== 100;
    options.cpu.overclock_numerator = cpuClock(args.userConfiguration);
    options.cpu.overclock_denominator = 100;
    options.main.setup_wizard_incomplete = false;
    options.main.confirm_power_off = false;
    options.main.save_state_on_exit = false;
    options.main.pause_on_focus_loss = false;
    options.main.emulation_speed = frameLimit(args.userConfiguration) / 100;
    options.display.vsync = args.userConfiguration?.graphics?.vsync === true;
    options.bios.search_directory = path("/User/bios");
    // Let DuckStation choose from the installed BIOS files by region and compatibility.
    options.bios.path_ntscu = "";
    options.bios.patch_fast_boot = true;
    options.folders.cache = path("/User/cache");
    options.folders.cheats = path("/User/cheats");
    options.folders.game_settings = path("/User/gamesettings");
    options.folders.input_profiles = path("/User/inputprofiles");
    options.folders.patches = path("/User/patches");
    options.folders.save_states = path("/User/savestates");
    options.folders.screenshots = path("/User/screenshots");
    options.folders.textures = path("/User/textures");
    options.folders.user_resources = path("/DuckStation/resources");
    options.memorycards.card1_type = args.storage.card1 ? "Shared" : "None";
    options.memorycards.card1_path = "polymer.mcd";
    options.memorycards.card2_type = "None";
    options.memorycards.directory = path("/User/memcards");
    options.memorycards.fast_forward_access = false;
    options.logging.log_level = "Info";
    options.logging.log_to_console = false;
    options.logging.log_to_file = true;
    options.logging.memory_card = true;
    options.logging.system = true;
    options.input_sources.sdl = true;
    options.gamelist.recursive_paths = path("/Game");
    options.autoupdater.check_at_startup = false;
    const settings = args.configuration.render()["settings.ini"];
    if (!settings) throw new TypeError("DuckStation configuration did not render settings.ini");
    args.filesystem.mount("/DuckStation/settings.ini", settings, { mode: "rw" });
    return {
        command: path("/DuckStation/duckstation-qt-x64-ReleaseLTCG.exe"),
        workingDirectory: args.filesystem.path("/DuckStation"),
        env: [
            ["USERPROFILE", args.filesystem.path("/User")],
            ["APPDATA", args.filesystem.path("/User/AppData/Roaming")],
            ["LOCALAPPDATA", args.filesystem.path("/User/AppData/Local")],
            ["TEMP", args.filesystem.path("/User/Temp")],
            ["TMP", args.filesystem.path("/User/Temp")],
        ],
        argv: ["-fastboot", "--", discPaths[args.initialDiscIndex]],
        isolate: true,
        hooks: { graphics: [graphicsApi(args.userConfiguration).hook], input: ["sdl3"] },
        surfaces: [{ index: 0 }],
    };
};

export const onParseHotkey = (expression: string) => HotkeyParser.parse(expression);

export const onPauseResume = (context: PauseResumeContext) => {
    context.actions.pause?.activate();
};

export const onDiscChange = (context: DiscChangeContext) => {
    const steps = context.index - currentDisc;
    const action = steps > 0 ? context.actions.next_disc : context.actions.previous_disc;
    for (let press = 0; press < Math.abs(steps); press++) action.activate();
    currentDisc = context.index;
};

export default { onPluginLoad, onUpdateContents, onConfigure, onLaunch, onParseHotkey, onPauseResume, onDiscChange };
