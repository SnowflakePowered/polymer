import runtime from "pkg:bin/win-x64";
import config from "pkg:plugin.toml";
import type { Configuration as HostConfiguration, Filesystem, Resource } from "polymer:api";

type Configuration = HostConfiguration;

type EmulatorLaunchArgs = {
    launchPath: string,
    console: string,
    configuration: Configuration,
    filesystem: Filesystem,
    storage: Readonly<{ vmu?: Resource }>,
};

function basename(path: string): string {
    const name = path.replaceAll("\\", "/").split("/").at(-1);
    if (!name) throw new TypeError(`path has no filename: ${path}`);
    return name;
}


export const onLaunch = (args: EmulatorLaunchArgs) => {
    const biosName = basename(config.plugin.config.bios);
    const flashName = basename(config.plugin.config.flash);
    const path = (virtualPath: string) => args.filesystem.path(virtualPath);

    args.filesystem.mountTree("/Flycast", runtime, { mode: "rx" });
    args.filesystem.mountScratch("/Flycast/data", { mode: "rwx", backing: "temporary" });
    args.filesystem.mountHostFile(`/Flycast/data/${biosName}`, config.plugin.config.bios, { mode: "r" });
    args.filesystem.mountHostFile(`/Flycast/data/${flashName}`, config.plugin.config.flash, { mode: "r" });
    if (args.storage.vmu) {
        args.filesystem.mountStorage("/Flycast/data/vmu_save_A1.bin", args.storage.vmu, {
            mode: "rw",
            format: "raw",
        });
    }
    const gamePath = args.filesystem.mountPackage("/Game");
    args.filesystem.mountScratch("/User", { mode: "rwx", backing: "temporary" });
    args.filesystem.setWorkingDirectory("/Flycast");
    args.filesystem.setProfile({
        home: "/User",
        roamingAppData: "/User/AppData/Roaming",
        localAppData: "/User/AppData/Local",
        temp: "/User/Temp",
    });
    for (const [name, bytes] of Object.entries(args.configuration.render())) {
        args.filesystem.mountFile(`/Flycast/${name}`, bytes, { mode: "rw" });
    }

    return {
        command: path("/Flycast/flycast.exe"),
        argv: [gamePath],
        isolate: false,
        hooks: { graphics: ["vulkan"], input: ["sdl3"] },
        surfaces: [{ index: 0, pattern: "Flycast" }],
    };
};

export default { onLaunch };
