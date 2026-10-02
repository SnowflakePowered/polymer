import runtime from "pkg:bin/win-x64";
import type { BatteryStorage, EmulatorLaunchArgs } from "polymer:api";

function saveName(romName: string): string {
    const extension = romName.lastIndexOf(".");
    return `${extension > 0 ? romName.slice(0, extension) : romName}.sav`;
}

export const onLaunch = (args: EmulatorLaunchArgs<unknown, BatteryStorage>) => {
    const gameName = args.launchSet[0].package.filename;
    const path = (virtualPath: string) => args.filesystem.path(virtualPath);

    args.filesystem.mount("/mGBA", runtime, { mode: "rx" });
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

export default { onLaunch };
