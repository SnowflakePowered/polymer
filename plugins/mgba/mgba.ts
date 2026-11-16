import { caches, config } from "polymer:plugin";
import { connect } from "polymer:database";
import { download } from "polymer:download";
import { Job } from "polymer:jobs";
import { osIdentifier } from "polymer:sys";
import { unarchive } from "polymer:unarchive";
import { EmulatorUpdates } from "jsr:@polymer-fe/emulator-updates@^0.1.0";
import type { BatteryStorage, ConfigurationResult, EmulatorLaunchArgs, Package, UserConfiguration } from "polymer:api";

function saveName(romName: string): string {
    const extension = romName.lastIndexOf(".");
    return `${extension > 0 ? romName.slice(0, extension) : romName}.sav`;
}

const runtime = new EmulatorUpdates({
    name: "mGBA",
    cache: caches.runtime,
    osIdentifier,
    channel: String(config.channel ?? "stable"),
    connect,
    download,
    unarchive,
    Job,
});

export const onPluginLoad = () => runtime.ready();

export const onUpdateContents = async () => runtime.update([await runtime.release("mgba")]);

export const onConfigure = (_pkg: Package, userConfiguration?: UserConfiguration): ConfigurationResult => ({
    schema: "mgba/v1",
    groups: [{
        key: "graphics",
        title: "Graphics",
        options: [{
            key: "vsync",
            title: "VSync",
            description: "Synchronize frames to the display's refresh rate to prevent tearing.",
            type: "bool",
            default: false,
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
    values: { graphics: { vsync: userConfiguration?.graphics?.vsync === true, frameLimit: frameLimit(userConfiguration) } },
});

function frameLimit(configuration?: UserConfiguration): number {
    const value = configuration?.graphics?.frameLimit;
    return typeof value === "number" && Number.isInteger(value) ? Math.min(150, Math.max(50, value)) : 100;
}

export const onLaunch = async (args: EmulatorLaunchArgs<{ qt: { videoSync: boolean, fpsTarget: number } }, BatteryStorage>) => {
    const gameName = args.launchSet[0].package.filename;
    const path = (virtualPath: string) => args.filesystem.path(virtualPath);

    args.filesystem.mount("/mGBA", await runtime.launch(), { mode: "rx" });
    args.configuration.options().qt.videoSync = args.userConfiguration?.graphics?.vsync === true;
    args.configuration.options().qt.fpsTarget = 60 * frameLimit(args.userConfiguration) / 100;
    args.filesystem.mount("/mGBA/config.ini", args.configuration.render()["config.ini"], { mode: "rw" });
    args.filesystem.mount("/User");
    const gamePath = args.filesystem.mount("/Game", args.launchSet[0].package, { mode: "rx" });
    if (args.storage.battery) {
        args.filesystem.mount(`/Game/${saveName(gameName)}`, args.storage.battery, {
            mode: "rw",
        });
    }

    return {
        command: path("/mGBA/mGBA.exe"),
        workingDirectory: args.filesystem.path("/mGBA"),
        env: [
            ["USERPROFILE", args.filesystem.path("/User")],
            ["APPDATA", args.filesystem.path("/User/AppData/Roaming")],
            ["LOCALAPPDATA", args.filesystem.path("/User/AppData/Local")],
            ["TEMP", args.filesystem.path("/User/Temp")],
            ["TMP", args.filesystem.path("/User/Temp")],
        ],
        argv: [gamePath],
        isolate: true,
        hooks: { graphics: ["opengl"], input: ["sdl3"] },
        surfaces: [{ index: 0, pattern: "mGBA", }],
    };
};

export default { onPluginLoad, onUpdateContents, onConfigure, onLaunch };
