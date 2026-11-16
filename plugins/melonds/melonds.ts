import type { BatteryStorage, ConfigurationResult, EmulatorLaunchArgs, Package, UserConfiguration } from "polymer:api";
import { caches, config } from "polymer:plugin";
import { connect } from "polymer:database";
import { download } from "polymer:download";
import { Job } from "polymer:jobs";
import { osIdentifier } from "polymer:sys";
import { unarchive } from "polymer:unarchive";
import { EmulatorUpdates } from "jsr:@polymer-fe/emulator-updates@^0.1.0";

const runtime = new EmulatorUpdates({
    name: "melonDS",
    cache: caches.runtime,
    osIdentifier,
    channel: String(config.channel ?? "stable"),
    connect,
    download,
    unarchive,
    Job,
});

export const onPluginLoad = () => runtime.ready();

export const onUpdateContents = async () => runtime.update([await runtime.release("melonds")]);

export const onConfigure = (_pkg: Package, userConfiguration?: UserConfiguration): ConfigurationResult => ({
    schema: "melonds/v1",
    groups: [{
        key: "graphics",
        title: "Graphics",
        options: [{
            key: "resolution",
            title: "Internal Resolution",
            description: "Multiple of the console's native resolution.",
            type: "int",
            default: 1,
            min: 1,
            max: MAX_RESOLUTION,
            choices: Array.from({ length: MAX_RESOLUTION }, (_, index) => ({ key: String(index + 1), label: `${256 * (index + 1)}×${192 * (index + 1)}` })),
        }, {
            key: "vsync",
            title: "VSync",
            description: "Synchronize frames to the display's refresh rate to prevent tearing.",
            type: "bool",
            default: true,
        }, {
            key: "frameLimit",
            title: "Frame Limiter",
            description: "Percentage of the console's native speed.",
            type: "int",
            default: 100,
            min: 50,
            max: 150,
        }],
    }],
    values: {
        graphics: {
            resolution: internalResolution(userConfiguration),
            vsync: userConfiguration?.graphics?.vsync !== false,
            frameLimit: frameLimit(userConfiguration),
        },
    },
});

const MAX_RESOLUTION = 8;

function internalResolution(configuration?: UserConfiguration): number {
    const value = configuration?.graphics?.resolution;
    return typeof value === "number" && Number.isInteger(value) ? Math.min(MAX_RESOLUTION, Math.max(1, value)) : 1;
}

function frameLimit(configuration?: UserConfiguration): number {
    const value = configuration?.graphics?.frameLimit;
    return typeof value === "number" && Number.isInteger(value) ? Math.min(150, Math.max(50, value)) : 100;
}

export const onLaunch = async (args: EmulatorLaunchArgs<{
    runtime: { TargetFPS: string },
    screen: { VSync: boolean },
    gl: { ScaleFactor: number },
    instance: { SaveFilePath: string, SavestatePath: string },
}, BatteryStorage>) => {
    const fs = args.filesystem;
    fs.mount("/melonDS", await runtime.launch(), { mode: "rx" });
    fs.mount("/melonDS/portable");
    fs.mount("/User");
    fs.mount("/User/states");
    const game = fs.mount("/Game", args.launchSet[0].package, { mode: "rx" });
    const filename = game.replaceAll("\\", "/").split("/").at(-1)!;
    const stem = filename.replace(/\.[^.]+$/, "");
    if (args.storage.battery) {
        fs.mount(`/User/saves/${stem}.sav`, args.storage.battery, { mode: "rw" });
    }
    const options = args.configuration.options();
    options.instance.SaveFilePath = JSON.stringify(fs.path("/User/saves"));
    options.instance.SavestatePath = JSON.stringify(fs.path("/User/states"));
    options.screen.VSync = args.userConfiguration?.graphics?.vsync !== false;
    options.gl.ScaleFactor = internalResolution(args.userConfiguration);
    options.runtime.TargetFPS = (60 * frameLimit(args.userConfiguration) / 100).toFixed(1);
    const settings = new TextDecoder().decode(args.configuration.render()["melonDS.toml"])
        .replace(/^\[GL\]\r?$/m, "[3D.GL]")
        .replace(/^\[(Window0|Keyboard|Joystick)\]\r?$/gm, "[Instance0.$1]");
    fs.mount("/melonDS/portable/melonDS.toml", new TextEncoder().encode(settings), { mode: "rw" });

    return {
        command: fs.path("/melonDS/melonDS.exe"),
        workingDirectory: fs.path("/melonDS"),
        argv: ["--boot", "always", game],
        env: [["USERPROFILE", fs.path("/User")], ["APPDATA", fs.path("/User/AppData/Roaming")],
            ["LOCALAPPDATA", fs.path("/User/AppData/Local")], ["TEMP", fs.path("/User/Temp")],
            ["TMP", fs.path("/User/Temp")]],
        isolate: true,
        hooks: { graphics: ["opengl"], input: ["sdl2"] },
        surfaces: [{ index: 0, pattern: "melonDS" }],
    };
};

export default { onPluginLoad, onUpdateContents, onConfigure, onLaunch };
