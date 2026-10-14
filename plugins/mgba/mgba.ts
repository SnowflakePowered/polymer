import { caches, config } from "polymer:plugin";
import { connect } from "polymer:database";
import { download } from "polymer:download";
import { Job } from "polymer:jobs";
import { osIdentifier } from "polymer:sys";
import { unarchive } from "polymer:unarchive";
import { EmulatorUpdates } from "jsr:@polymer-fe/emulator-updates@^0.1.0";
import type { BatteryStorage, EmulatorLaunchArgs } from "polymer:api";

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

export const onLaunch = async (args: EmulatorLaunchArgs<unknown, BatteryStorage>) => {
    const gameName = args.launchSet[0].package.filename;
    const path = (virtualPath: string) => args.filesystem.path(virtualPath);

    args.filesystem.mount("/mGBA", await runtime.launch(), { mode: "rx" });
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

export default { onPluginLoad, onUpdateContents, onLaunch };
