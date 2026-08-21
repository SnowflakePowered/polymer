import config from "pkg:plugin.toml";

type Configuration = { options(): unknown };
type Resource = Readonly<{ kind: "tree" | "file", path?: string }>;
type Filesystem = {
    mountHostFile(target: string, source: string, options: { mode: string }): void,
    mountHostTree(target: string, source: string, options: { mode: string }): void,
    mountTree(target: string, source: Resource | URL | string, options: { mode: string }): void,
    mountFile(target: string, source: Resource | URL | string | Uint8Array, options: { mode: string }): void,
    mountNand(target: string): void,
    mountScratch(target: string, options: { mode: string, backing: "memory" | "temporary" }): void,
    path(path: string): string,
    setWorkingDirectory(path: string): void,
    setProfile(profile: { home?: string, roamingAppData?: string, localAppData?: string, temp?: string }): void,
};
type SetupLaunchArgs = {
    launchPath: string,
    console: string,
    portAssignments: { [group: string]: string[] },
    configuration: Configuration,
    filesystem: Filesystem,
    resources: { runtime: Resource },
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
    const biosName = basename(config.plugin.config.bios);
    const gameName = basename(args.launchPath);
    const path = (virtualPath: string) => args.filesystem.path(virtualPath);

    args.filesystem.mountTree("/DuckStation", args.resources.runtime, { mode: "rx" });
    args.filesystem.mountFile("/DuckStation/portable.txt", new Uint8Array(), { mode: "r" });
    args.filesystem.mountFile("/DuckStation/duckstation.log", new Uint8Array(), { mode: "rw" });
    args.filesystem.mountScratch("/User", { mode: "rwx", backing: "temporary" });
    args.filesystem.mountScratch("/User/generated", { mode: "rwx", backing: "temporary" });
    args.filesystem.mountNand("/User/memcards");
    args.filesystem.mountHostFile(`/User/bios/${biosName}`, config.plugin.config.bios, { mode: "r" });
    args.filesystem.mountHostTree("/Game", dirname(args.launchPath), { mode: "rx" });
    args.filesystem.setWorkingDirectory("/DuckStation");
    args.filesystem.setProfile({
        home: "/User",
        roamingAppData: "/User/AppData/Roaming",
        localAppData: "/User/AppData/Local",
        temp: "/User/Temp",
    });

    const settings = [
        "[Main]",
        "SettingsVersion = 3",
        "SetupWizardIncomplete = false",
        "ConfirmPowerOff = false",
        "SaveStateOnExit = false",
        "PauseOnFocusLoss = false",
        "",
        "[BIOS]",
        `SearchDirectory = ${path("/User/bios")}`,
        `PathNTSCU = ${biosName}`,
        "PatchFastBoot = true",
        "",
        "[Folders]",
        `Cache = ${path("/User/cache")}`,
        `Cheats = ${path("/User/cheats")}`,
        `GameSettings = ${path("/User/gamesettings")}`,
        `InputProfiles = ${path("/User/inputprofiles")}`,
        `Patches = ${path("/User/patches")}`,
        `SaveStates = ${path("/User/savestates")}`,
        `Screenshots = ${path("/User/screenshots")}`,
        `Textures = ${path("/User/textures")}`,
        `UserResources = ${path("/DuckStation/resources")}`,
        "",
        "[MemoryCards]",
        "Card1Type = Shared",
        "Card1Path = snowglass.mcd",
        "Card2Type = None",
        `Directory = ${path("/User/memcards")}`,
        "FastForwardAccess = false",
        "",
        "[Logging]",
        "LogLevel = Info",
        "LogToConsole = false",
        "LogToFile = true",
        "MemoryCard = true",
        "System = true",
        "",
        "[InputSources]",
        "SDL = true",
        "",
    ].join("\n");
    args.filesystem.mountFile("/DuckStation/settings.ini", new TextEncoder().encode(settings), { mode: "rw" });

    // The generated configuration remains available for future user-facing options. The
    // invocation-owned settings above supply the path-sensitive runtime values today.
    args.configuration.options();
    return {
        command: path("/DuckStation/duckstation-qt-x64-ReleaseLTCG.exe"),
        argv: ["-batch", "-fullscreen", "-fastboot", "--", path(`/Game/${gameName}`)],
        configRoot: path("/User/generated"),
        isolate: true,
        hooks: ["d3d11"],
        config: args.configuration,
        surfaces: [{ index: 0, pattern: "DuckStation|Crash Bandicoot" }],
    };
};

export default { setupLaunch };
