import { caches, config } from "polymer:plugin";
import { connect } from "polymer:database";
import { download } from "polymer:download";
import { Job } from "polymer:jobs";
import { graphicsApis, osIdentifier } from "polymer:sys";
import { unarchive } from "polymer:unarchive";
import { EmulatorUpdates } from "jsr:@polymer-fe/emulator-updates@^0.1.0";
import type { ConfigurationResult, EmulatorLaunchArgs, GraphicsHook, Package, UserConfiguration } from "polymer:api";

const runtime = new EmulatorUpdates({
    name: "Ymir",
    cache: caches.runtime,
    osIdentifier,
    channel: String(config.channel ?? "stable"),
    connect,
    download,
    unarchive,
    Job,
});

export const onPluginLoad = () => runtime.ready();

export const onUpdateContents = async () => runtime.update([await runtime.release("ymir")]);

const GRAPHICS_APIS = ([
    { hook: "d3d11", label: "Direct3D 11", value: "direct3d11" },
    { hook: "d3d12", label: "Direct3D 12", value: "direct3d12" },
    { hook: "vulkan", label: "Vulkan", value: "vulkan" },
    { hook: "opengl", label: "OpenGL", value: "opengl" },
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

export const onConfigure = (_pkg: Package, userConfiguration?: UserConfiguration): ConfigurationResult => ({
    schema: "ymir/v1",
    groups: GRAPHICS_APIS.length > 1 ? [{ key: "graphics", title: "Graphics", options: [GRAPHICS_API_OPTION] }] : [],
    values: { graphics: { api: graphicsApi(userConfiguration)?.hook ?? null } },
});

export const onLaunch = async (args: EmulatorLaunchArgs<{
    system: { InternalBackupRAMImagePath: string },
    ipl: { Path: string },
}>) => {
    const bios = (await args.firmwareStore.list()).find(file => file.byteLength === 512 * 1024);
    if (!bios) throw new Error("Ymir requires a Saturn BIOS. Import a Saturn BIOS into Polymer first.");
    const fs = args.filesystem;
    fs.mount("/Ymir", await runtime.launch(), { mode: "rx" });
    fs.mount("/Profile");
    fs.mount("/Profile/state", caches.saves, { mode: "rw" });
    fs.mount("/Profile/bios.bin", await args.firmwareStore.open(bios.label), { mode: "r" });
    fs.mount("/User");
    const game = fs.mount("/Game", args.launchSet[0].package, { mode: "rx" });
    const api = graphicsApi(args.userConfiguration);
    const options = args.configuration.options();
    options.system.InternalBackupRAMImagePath = JSON.stringify(fs.path("/Profile/state/bup-int.bin"));
    options.ipl.Path = JSON.stringify(fs.path("/Profile/bios.bin"));
    // The host's INI emitter can carry TOML scalar/array literals. Expand the
    // native table paths here because host target paths identify separate nodes.
    const rendered = new TextDecoder().decode(args.configuration.render()["Ymir.toml"])
        .replace("[IPL]", "[System.IPL]")
        .replace(/\[Type([12])\]/g, "[Input.Port$1]")
        .replace(/\[Port([12])\]/g, "[Input.Port$1.ControlPad.Binds]");
    const config = `${rendered}\n[General]\nPauseWhenUnfocused = false\nCheckForUpdates = false\n`;
    fs.mount("/Profile/Ymir.toml", new TextEncoder().encode(config), { mode: "rw" });
    return {
        command: fs.path("/Ymir/ymir-sdl3.exe"),
        workingDirectory: fs.path("/Ymir"),
        argv: ["--profile", fs.path("/Profile"), "--disc", game],
        env: [["SDL_RENDER_DRIVER", api.value], ["USERPROFILE", fs.path("/User")],
            ["APPDATA", fs.path("/User/AppData/Roaming")], ["LOCALAPPDATA", fs.path("/User/AppData/Local")],
            ["TEMP", fs.path("/User/Temp")], ["TMP", fs.path("/User/Temp")]],
        isolate: true,
        hooks: { graphics: [api.hook], input: ["sdl3"] },
        surfaces: [{ index: 0, pattern: "Ymir" }],
    };
};

export default { onPluginLoad, onUpdateContents, onConfigure, onLaunch };
