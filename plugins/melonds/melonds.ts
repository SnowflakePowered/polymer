import runtime from "pkg:bin/win-x64";
import type { BatteryStorage, EmulatorLaunchArgs } from "polymer:api";

export const onLaunch = (args: EmulatorLaunchArgs<
    { runtime: { SaveFilePath: string, SavestatePath: string } },
    BatteryStorage
>) => {
    const fs = args.filesystem;
    fs.mount("/melonDS", runtime, { mode: "rx" });
    fs.mount("/melonDS/portable");
    fs.mount("/User");
    fs.mount("/User/states");
    const game = fs.mount("/Game", args.launchSet[0].package, { mode: "rx" });
    const filename = game.replaceAll("\\", "/").split("/").at(-1)!;
    const stem = filename.replace(/\.[^.]+$/, "");
    if (args.storage.battery) {
        fs.mount(`/User/saves/${stem}.sav`, args.storage.battery, { mode: "rw" });
    }
    const options = args.configuration.options().runtime;
    options.SaveFilePath = fs.path("/User/saves");
    options.SavestatePath = fs.path("/User/states");
    // 1.1 migrates this legacy file to melonDS.toml on first launch.
    for (const [name, bytes] of Object.entries(args.configuration.render())) {
        fs.mount(`/melonDS/portable/${name}`, bytes, { mode: "rw" });
    }

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

export default { onLaunch };
