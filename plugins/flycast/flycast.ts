import { caches, config } from "polymer:plugin";
import { connect } from "polymer:database";
import { download } from "polymer:download";
import { Job } from "polymer:jobs";
import { graphicsApis, osIdentifier } from "polymer:sys";
import { unarchive } from "polymer:unarchive";
import { EmulatorUpdates } from "jsr:@polymer-fe/emulator-updates@^0.1.0";
import discScript from "pkg:disc.lua";
import type { ConfigurationResult, DiscChangeContext, EmulatorLaunchArgs, Package, Storage, GraphicsHook, UserConfiguration } from "polymer:api";

type Options = { runtime: { resolution: number, renderer: string, sh4_clock: number, vsync: boolean } };

const DISC_REQUEST = "/Flycast/polymer-disc.txt";
let discPaths: string[] = [];

const runtime = new EmulatorUpdates({
    name: "Flycast",
    cache: caches.runtime,
    osIdentifier,
    channel: String(config.channel ?? "stable"),
    connect,
    download,
    unarchive,
    Job,
});

export const onPluginLoad = () => runtime.ready();

export const onUpdateContents = async () => runtime.update([await runtime.release("flycast")]);

export const onConfigure = (_pkg: Package, userConfiguration?: UserConfiguration): ConfigurationResult => ({
    schema: "flycast/v1",
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
            choices: Array.from({ length: MAX_RESOLUTION }, (_, index) => ({ key: String(index + 1), label: `${640 * (index + 1)}×${480 * (index + 1)}` })),
        }, {
            key: "vsync",
            title: "VSync",
            description: "Synchronize frames to the display's refresh rate to prevent tearing.",
            type: "bool",
            default: true,
        }],
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
        },
        cpu: { clock: cpuClock(userConfiguration) },
    },
});

const GRAPHICS_APIS = ([
    { hook: "vulkan", label: "Vulkan", value: "Vulkan" },
    { hook: "d3d11", label: "Direct3D 11", value: "DirectX11" },
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

const MAX_CPU_CLOCK = 150;

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

export const onLaunch = async (args: EmulatorLaunchArgs<Options, Readonly<{ vmu?: Storage }>>) => {
    const path = (virtualPath: string) => args.filesystem.path(virtualPath);

    args.filesystem.mount("/Flycast", await runtime.launch(), { mode: "rx" });
    args.filesystem.mount("/Flycast/data");
    for (const name of ["dc_boot.bin", "dc_flash.bin"]) {
        const file = await args.firmwareStore.open(name);
        args.filesystem.mount(`/Flycast/data/${name}`, file, { mode: "r" });
    }
    if (args.storage.vmu) {
        args.filesystem.mount("/Flycast/data/vmu_save_A1.bin", args.storage.vmu, {
            mode: "rw",
            format: "raw",
        });
    }
    discPaths = args.launchSet.length > 1
        ? args.launchSet.map((disc, index) => args.filesystem.mount(`/Game/Disc${index}`, disc.package, { mode: "rx" }))
        : [args.filesystem.mount("/Game", args.launchSet[0].package, { mode: "rx" })];
    const gamePath = discPaths[args.initialDiscIndex];
    if (discPaths.length > 1) {
        args.filesystem.mount("/Flycast/flycast.lua", discScript, { mode: "r" });
        args.filesystem.mount(DISC_REQUEST, new TextEncoder().encode(gamePath), { mode: "r" });
    }
    args.filesystem.mount("/User");
    args.configuration.options().runtime.resolution = 480 * internalResolution(args.userConfiguration);
    args.configuration.options().runtime.renderer = graphicsApi(args.userConfiguration).value;
    args.configuration.options().runtime.sh4_clock = Math.round(2 * cpuClock(args.userConfiguration));
    args.configuration.options().runtime.vsync = args.userConfiguration?.graphics?.vsync !== false;
    for (const [name, bytes] of Object.entries(args.configuration.render())) {
        args.filesystem.mount(`/Flycast/${name}`, bytes, { mode: "rw" });
    }

    return {
        command: path("/Flycast/flycast.exe"),
        workingDirectory: args.filesystem.path("/Flycast"),
        env: [
            ["USERPROFILE", args.filesystem.path("/User")],
            ["APPDATA", args.filesystem.path("/User/AppData/Roaming")],
            ["LOCALAPPDATA", args.filesystem.path("/User/AppData/Local")],
            ["TEMP", args.filesystem.path("/User/Temp")],
            ["TMP", args.filesystem.path("/User/Temp")],
            ["POLYMER_DISC_REQUEST", path(DISC_REQUEST)],
        ],
        argv: [gamePath],
        isolate: false,
        hooks: { graphics: [graphicsApi(args.userConfiguration).hook], input: ["sdl2"] },
        surfaces: [{ index: 0, pattern: "Flycast" }],
    };
};

export const onDiscChange = (context: DiscChangeContext) => {
    context.filesystem.mount(DISC_REQUEST, new TextEncoder().encode(discPaths[context.index]), { mode: "r" });
    context.filesystem.flush();
};

export default { onPluginLoad, onUpdateContents, onConfigure, onLaunch, onDiscChange };
