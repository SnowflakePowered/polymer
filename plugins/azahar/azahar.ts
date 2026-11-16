import { caches, config } from "polymer:plugin";
import { connect } from "polymer:database";
import { download } from "polymer:download";
import { Job } from "polymer:jobs";
import { graphicsApis, osIdentifier } from "polymer:sys";
import { unarchive } from "polymer:unarchive";
import { EmulatorUpdates } from "jsr:@polymer-fe/emulator-updates@^0.1.0";
import type { ConfigurationResult, EmulatorLaunchArgs, Package, PauseResumeContext, Storage, GraphicsHook, UserConfiguration } from "polymer:api";
import { HotkeyParser } from "pkg:hotkeys.ts";

const runtime = new EmulatorUpdates({
    name: "Azahar",
    cache: caches.runtime,
    osIdentifier,
    channel: String(config.channel ?? "stable"),
    connect,
    download,
    unarchive,
    Job,
});

export const onPluginLoad = () => runtime.ready();

export const onUpdateContents = async () => runtime.update([await runtime.release("azahar")]);

export const onConfigure = (_pkg: Package, userConfiguration?: UserConfiguration): ConfigurationResult => ({
    schema: "azahar/v1",
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
            choices: Array.from({ length: MAX_RESOLUTION }, (_, index) => ({ key: String(index + 1), label: `${400 * (index + 1)}×${240 * (index + 1)}` })),
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
            vsync: userConfiguration?.graphics?.vsync !== false,
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
    default: true,
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
    { hook: "vulkan", label: "Vulkan", value: 2 },
    { hook: "opengl", label: "OpenGL", value: 1 },
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

export const onLaunch = async (args: EmulatorLaunchArgs<
    { storage: { nand_directory: string, sdmc_directory: string }, renderer: { resolution_factor: number, graphics_api: number, use_vsync: boolean, frame_limit: number }, core: { cpu_clock_percentage: number } },
    Readonly<{ ctrnand?: Storage }>
>) => {
    if (!args.storage.ctrnand) {
        throw new Error("Select a Nintendo 3DS NAND and save repository before launching Azahar.");
    }
    const fs = args.filesystem;
    fs.mount("/Azahar", await runtime.launch(), { mode: "rx" });
    fs.mount("/Azahar/user");
    // Polymer's 3DS volume contains both nand/ and sdmc/, including title tickets.
    fs.mount("/Storage", args.storage.ctrnand, { mode: "rwx" });
    fs.mount("/User");
    const game = fs.mount("/Game", args.launchSet[0].package, { mode: "rx" });
    const options = args.configuration.options().storage;
    options.nand_directory = fs.path("/Storage/nand");
    options.sdmc_directory = fs.path("/Storage/sdmc");
    args.configuration.options().renderer.resolution_factor = internalResolution(args.userConfiguration);
    args.configuration.options().renderer.graphics_api = graphicsApi(args.userConfiguration).value;
    args.configuration.options().renderer.use_vsync = args.userConfiguration?.graphics?.vsync !== false;
    args.configuration.options().renderer.frame_limit = frameLimit(args.userConfiguration);
    args.configuration.options().core.cpu_clock_percentage = cpuClock(args.userConfiguration);
    for (const [name, bytes] of Object.entries(args.configuration.render({ ini: { quote: "strings", escape: true } }))) {
        fs.mount(`/Azahar/user/config/${name}`, bytes, { mode: "rw" });
    }
    return {
        command: fs.path("/Azahar/azahar.exe"),
        workingDirectory: fs.path("/Azahar"),
        argv: [game],
        env: [["USERPROFILE", fs.path("/User")], ["APPDATA", fs.path("/User/AppData/Roaming")],
            ["LOCALAPPDATA", fs.path("/User/AppData/Local")], ["TEMP", fs.path("/User/Temp")],
            ["TMP", fs.path("/User/Temp")]],
        isolate: false,
        hooks: { graphics: [graphicsApi(args.userConfiguration).hook], input: ["sdl2"] },
        surfaces: [{ index: 0, pattern: "^Azahar [^|]*\\| [^|]*$" }, { index: 1, pattern: "\\| Secondary Window$" }],
    };
};

export const onParseHotkey = (expression: string) => HotkeyParser.parse(expression);

export const onPauseResume = (context: PauseResumeContext) => {
    context.actions.pause?.activate();
};

export default { onPluginLoad, onUpdateContents, onConfigure, onLaunch, onParseHotkey, onPauseResume };
