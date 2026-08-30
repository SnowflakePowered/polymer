import config from "pkg:plugin.toml";
import type { Configuration as HostConfiguration, Filesystem, Resource } from "../plugin-api.ts";

type Configuration = HostConfiguration;

type EmulatorLaunchArgs = {
    launchPath: string,
    console: string,
    configuration: Configuration,
    filesystem: Filesystem,
    resources: { runtime: Resource },
    storage: Readonly<{ vmu?: Resource }>,
};

function basename(path: string): string {
    const name = path.replaceAll("\\", "/").split("/").at(-1);
    if (!name) throw new TypeError(`path has no filename: ${path}`);
    return name;
}

function dirname(path: string): string {
    const normalized = path.replaceAll("\\", "/");
    const separator = normalized.lastIndexOf("/");
    if (separator <= 0) throw new TypeError(`path has no parent: ${path}`);
    return normalized.slice(0, separator);
}

export const onEmulatorLaunch = (args: EmulatorLaunchArgs) => {
    const gameName = basename(args.launchPath);
    const discName = gameName.replace(/ \(Track \d+\)\.bin$/i, ".cue");
    const biosName = basename(config.plugin.config.bios);
    const flashName = basename(config.plugin.config.flash);
    const path = (virtualPath: string) => args.filesystem.path(virtualPath);

    args.filesystem.mountTree("/Flycast", args.resources.runtime, { mode: "rx" });
    args.filesystem.mountScratch("/Flycast/data", { mode: "rwx", backing: "temporary" });
    args.filesystem.mountFile(
        "/Flycast/emu.cfg",
        new TextEncoder().encode([
            "[window]",
            "fullscreen = no",
            "maximized = no",
            "width = 960",
            "height = 720",
            "",
        ].join("\n")),
        { mode: "rw" },
    );
    args.filesystem.mountHostFile(`/Flycast/data/${biosName}`, config.plugin.config.bios, { mode: "r" });
    args.filesystem.mountHostFile(`/Flycast/data/${flashName}`, config.plugin.config.flash, { mode: "r" });
    if (args.storage.vmu) {
        args.filesystem.mountStorage("/Flycast/data/vmu_save_A1.bin", args.storage.vmu, {
            mode: "rw",
            format: "raw",
        });
    }
    args.filesystem.mountHostTree("/Game", dirname(args.launchPath), { mode: "rx" });
    args.filesystem.mountScratch("/User", { mode: "rwx", backing: "temporary" });
    args.filesystem.mountScratch("/User/generated", { mode: "rwx", backing: "temporary" });
    args.filesystem.setWorkingDirectory("/Flycast");
    args.filesystem.setProfile({
        home: "/User",
        roamingAppData: "/User/AppData/Roaming",
        localAppData: "/User/AppData/Local",
        temp: "/User/Temp",
    });
    for (const [name, bytes] of Object.entries(args.configuration.render())) {
        args.filesystem.mountFile(`/User/generated/${name}`, bytes, { mode: "rw" });
    }

    return {
        command: path("/Flycast/flycast.exe"),
        argv: [
            "-config", "config:PerGameVmu=no",
            "-config", "config:Dreamcast.AutoLoadState=no",
            "-config", "config:Dreamcast.AutoSaveState=no",
            "-config", "config:FastGDRomLoad=yes",
            "-config", "pvr:rend=2",
            "-config", "window:fullscreen=no",
            path(`/Game/${discName}`),
        ],
        isolate: true,
        hooks: ["d3d11"],
        surfaces: [{ index: 0, pattern: "Flycast|Sonic Adventure" }],
    };
};

export default { onEmulatorLaunch };
