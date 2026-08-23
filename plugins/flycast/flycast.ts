import config from "pkg:plugin.toml";

type Configuration = {
    render(): Readonly<Record<string, Uint8Array>>,
};

type Resource = Readonly<{ kind: "tree" | "file", path?: string }>;

type Filesystem = {
    mountHostFile(target: string, source: string, options: { mode: string }): void,
    mountHostTree(target: string, source: string, options: { mode: string }): void,
    mountTree(target: string, source: Resource | URL | string, options: { mode: string }): void,
    mountFile(target: string, source: Resource | URL | string | Uint8Array, options: { mode: string }): void,
    mountStorage(target: string, source: Resource, options: { mode: string, format: string }): void,
    mountScratch(target: string, options: { mode: string, backing: "memory" | "temporary" }): void,
    path(path: string): string,
    setWorkingDirectory(path: string): void,
    setProfile(profile: {
        home?: string,
        roamingAppData?: string,
        localAppData?: string,
        temp?: string,
    }): void,
};

type SetupLaunchArgs = {
    launchPath: string,
    console: string,
    portAssignments: { [group: string]: string[] },
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

export const setupLaunch = (args: SetupLaunchArgs) => {
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
        configRoot: path("/User/generated"),
        isolate: true,
        hooks: ["d3d11"],
        config: args.configuration,
        surfaces: [{ index: 0, pattern: "Flycast|Sonic Adventure" }],
    };
};

export default { setupLaunch };
