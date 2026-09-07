import runtime from "pkg:bin/win-x64";
import type { BatteryStorage, Filesystem } from "polymer:api";

type EmulatorLaunchArgs = {
    launchPath: string,
    filesystem: Filesystem,
    storage: BatteryStorage,
};

function basename(path: string): string {
    const name = path.replaceAll("\\", "/").split("/").at(-1);
    if (!name) throw new TypeError(`path has no filename: ${path}`);
    return name;
}

function saveName(romName: string): string {
    const extension = romName.lastIndexOf(".");
    return `${extension > 0 ? romName.slice(0, extension) : romName}.sav`;
}

export const onLaunch = (args: EmulatorLaunchArgs) => {
    const gameName = basename(args.launchPath);
    const path = (virtualPath: string) => args.filesystem.path(virtualPath);

    args.filesystem.mountTree("/mGBA", runtime, { mode: "rx" });
    args.filesystem.mountScratch("/Game", { mode: "rwx", backing: "temporary" });
    args.filesystem.mountScratch("/User", { mode: "rwx", backing: "temporary" });
    args.filesystem.mountHostFile(`/Game/${gameName}`, args.launchPath, { mode: "r" });
    if (args.storage.battery) {
        args.filesystem.mountStorage(`/Game/${saveName(gameName)}`, args.storage.battery, {
            mode: "rw",
        });
    }
    args.filesystem.setWorkingDirectory("/mGBA");
    args.filesystem.setProfile({
        home: "/User",
        roamingAppData: "/User/AppData/Roaming",
        localAppData: "/User/AppData/Local",
        temp: "/User/Temp",
    });

    return {
        command: path("/mGBA/mGBA.exe"),
        argv: [path(`/Game/${gameName}`)],
        isolate: true,
        hooks: { graphics: ["opengl"], input: ["sdl3"] },
        surfaces: [{ index: 0, pattern: "mGBA", resolution: [640, 480] }],
    };
};

export default { onLaunch };
