import { caches, config } from "polymer:plugin";
import { connect } from "polymer:database";
import { download } from "polymer:download";
import { Job } from "polymer:jobs";
import { graphicsApis, osIdentifier } from "polymer:sys";
import { unarchive } from "polymer:unarchive";
import { EmulatorUpdates } from "jsr:@polymer-fe/emulator-updates@^0.1.0";
import { Rpcs3Configuration } from "pkg:configuration.ts";
import type { ConfigurationResult, EmulatorLaunchArgs, GraphicsHook, Package, SaveCheckpointContext, Storage, UserConfiguration } from "polymer:api";

const runtime = new EmulatorUpdates({
    name: "RPCS3",
    cache: caches.runtime,
    osIdentifier,
    channel: String(config.channel ?? "stable"),
    connect,
    download,
    unarchive,
    Job,
});

export const onPluginLoad = () => runtime.ready();

export const onUpdateContents = async () => runtime.update([await runtime.release("rpcs3")]);

const GRAPHICS_APIS = ([
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

const SHADER_MODES = [
    { key: "Async Recompiler with Shader Interpreter", label: "Async with Shader Interpreter" },
    { key: "Async Recompiler (multi-threaded)", label: "Async (multi-threaded)" },
    { key: "Legacy Recompiler (single-threaded)", label: "Legacy (single-threaded)" },
    { key: "Shader Interpreter only", label: "Shader Interpreter only" },
];

function shaderMode(configuration?: UserConfiguration): string {
    const value = configuration?.graphics?.shaderMode;
    return SHADER_MODES.find(mode => mode.key === value)?.key ?? SHADER_MODES[0].key;
}

export const onConfigure = (_pkg: Package, userConfiguration?: UserConfiguration): ConfigurationResult => ({
    schema: "rpcs3/v1",
    groups: [{
        key: "graphics",
        title: "Graphics",
        options: [...(GRAPHICS_APIS.length > 1 ? [GRAPHICS_API_OPTION] : []), {
            key: "shaderMode",
            title: "Shader Mode",
            description: "How shaders are compiled. The async modes reduce stutter.",
            type: "select",
            default: SHADER_MODES[0].key,
            choices: SHADER_MODES,
        }],
    }],
    values: {
        graphics: { api: graphicsApi(userConfiguration)?.hook ?? null, shaderMode: shaderMode(userConfiguration) },
    },
});

export const onLaunch = async (args: EmulatorLaunchArgs<{ video: { Renderer: string, "Shader Mode": string } }, Readonly<{ hdd?: Storage, flash?: Storage }>>) => {
    if (!args.storage.hdd) {
        throw new Error("Select a PlayStation 3 NAND and save repository for the HDD, and install system software before launching RPCS3.");
    }
    const fs = args.filesystem;
    fs.mount("/RPCS3", await runtime.launch(), { mode: "rx" });
    fs.mount("/Storage/hdd", args.storage.hdd, { mode: "rwx" });
    if (args.storage.flash) fs.mount("/Storage/flash", args.storage.flash, { mode: "rwx" });
    // The PS3 NAND contains both HDD and flash trees; a separate flash selection
    // is optional and overrides only the firmware partitions.
    const flash = args.storage.flash ? "/Storage/flash" : "/Storage/hdd";
    fs.mount("/RPCS3/cache", caches.cache, { mode: "rwx" });
    for (const dir of ["config", "GuiConfigs", "captures", "savestates", "dev_hdd1", "log"]) fs.mount(`/RPCS3/${dir}`);
    for (const file of ["RPCS3.buf", "RPCS3.log", "RPCS3.log.gz", "TTY.log"]) fs.mount(`/RPCS3/${file}`, new Uint8Array(), { mode: "rw" });
    fs.mount("/User");
    fs.mount("/User/Temp");
    fs.mount("/User/AppData/Local", caches.driver, { mode: "rwx" });
    const installed = args.launchSet[0].package.installed;
    if (!installed && args.launchSet[0].package.uri.startsWith("artifact://")) {
        throw new Error("The installed PS3 title has no bootable location.");
    }
    const game = installed
        ? fs.path(`/Storage/${installed.device}/${installed.path}/${installed.entrypoint}`)
        : fs.mount("/Game", args.launchSet[0].package, { mode: "rx" });
    const vfs = {
        "/dev_hdd0/": `${fs.path("/Storage/hdd/dev_hdd0")}/`,
        "/dev_hdd1/": `${fs.path("/RPCS3/dev_hdd1")}/`,
        "/dev_flash/": `${fs.path(`${flash}/dev_flash`)}/`,
        "/dev_flash2/": `${fs.path(`${flash}/dev_flash2`)}/`,
        "/dev_flash3/": `${fs.path(`${flash}/dev_flash3`)}/`,
    };
    fs.mount("/RPCS3/config/vfs.yml", new TextEncoder().encode(JSON.stringify(vfs)), { mode: "rw" });
    const api = graphicsApi(args.userConfiguration);
    args.configuration.options().video.Renderer = api.value;
    args.configuration.options().video["Shader Mode"] = shaderMode(args.userConfiguration);
    Rpcs3Configuration.mount(args.configuration, fs);
    return {
        command: fs.path("/RPCS3/rpcs3.exe"),
        workingDirectory: fs.path("/RPCS3"),
        argv: ["--no-gui", "--user-id", "00000001", "--config", fs.path("/RPCS3/config.yml"), game],
        env: [["USERPROFILE", fs.path("/User")], ["APPDATA", fs.path("/User/AppData/Roaming")],
            ["LOCALAPPDATA", fs.path("/User/AppData/Local")], ["TEMP", fs.path("/User/Temp")],
            ["TMP", fs.path("/User/Temp")]],
        isolate: false,
        hooks: { graphics: [api.hook], input: ["sdl3"] },
        surfaces: [{ index: 0, pattern: `RPCS3|FPS.*${api.value}` }],
    };
};

export const onSaveCheckpoint = ({ storage, changed }: SaveCheckpointContext) =>
    storage !== "hdd" || !changed.some(({ path }) =>
        path.split("/").some((part) => part.startsWith(".working_") || part.startsWith(".backup_")));

export default { onPluginLoad, onUpdateContents, onConfigure, onLaunch, onSaveCheckpoint };
