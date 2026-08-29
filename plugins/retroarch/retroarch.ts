import config from "pkg:plugin.toml";

type Configuration = {
    options(): { core: { placeholder: boolean } },
    render(bindings: readonly GamepadBinding[]): Readonly<Record<string, Uint8Array>>,
};

type GamepadBinding = Readonly<{
    target: Readonly<{ group: string, port: number }>,
    gamepad: number,
}>;

type Resource = Readonly<{ kind: "tree" | "file", path?: string }>;

type Filesystem = {
    mountHostFile(target: string, source: string, options: { mode: string }): void,
    mountHostTree(target: string, source: string, options: { mode: string }): void,
    mountTree(target: string, source: Resource | URL | string, options: { mode: string }): void,
    mountFile(target: string, source: Resource | URL | string | Uint8Array, options: { mode: string }): void,
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
    gamepadBindings: readonly GamepadBinding[],
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

function quoted(value: string): string {
    return `"${value.replaceAll("\\", "\\\\").replaceAll("\"", "\\\"")}"`;
}

export const setupLaunch = (args: SetupLaunchArgs) => {
    const biosName = basename(config.plugin.config.bios);
    const gameName = basename(args.launchPath);

    args.filesystem.mountTree("/RetroArch", args.resources.runtime, { mode: "rx" });
    args.filesystem.mountScratch("/User", { mode: "rwx", backing: "temporary" });
    args.filesystem.mountScratch("/User/saves", { mode: "rwx", backing: "temporary" });
    args.filesystem.mountScratch("/User/states", { mode: "rwx", backing: "temporary" });
    args.filesystem.mountScratch("/User/logs", { mode: "rwx", backing: "temporary" });
    args.filesystem.mountScratch("/User/system/pcsx2/memcards", { mode: "rwx", backing: "temporary" });
    args.filesystem.mountScratch("/Generated", { mode: "rwx", backing: "temporary" });
    args.filesystem.mountHostTree("/User/system/pcsx2/bios", dirname(config.plugin.config.bios), { mode: "rx" });
    args.filesystem.mountHostFile(`/Game/${gameName}`, args.launchPath, { mode: "r" });
    args.filesystem.setWorkingDirectory("/RetroArch");
    args.filesystem.setProfile({
        home: "/User",
        roamingAppData: "/User/AppData/Roaming",
        localAppData: "/User/AppData/Local",
        temp: "/User/Temp",
    });

    const runtimeConfig = [
        `config_save_on_exit = "false"`,
        `core_info_cache_enable = "false"`,
        `core_options_path = ${quoted(args.filesystem.path("/User/core-options.cfg"))}`,
        `system_directory = ${quoted(args.filesystem.path("/User/system"))}`,
        `savefile_directory = ${quoted(args.filesystem.path("/User/saves"))}`,
        `savestate_directory = ${quoted(args.filesystem.path("/User/states"))}`,
        `log_dir = ${quoted(args.filesystem.path("/User/logs"))}`,
        `libretro_directory = ${quoted(args.filesystem.path("/RetroArch/cores"))}`,
        `libretro_info_path = ${quoted(args.filesystem.path("/RetroArch/info"))}`,
        `assets_directory = ${quoted(args.filesystem.path("/RetroArch/assets"))}`,
        `video_driver = "d3d11"`,
        `video_fullscreen = "true"`,
        `input_autodetect_enable = "true"`,
        `pause_nonactive = "false"`,
        `quit_on_close_content = "1"`,
        "",
    ].join("\n");
    args.filesystem.mountFile(
        "/User/retroarch.cfg",
        new TextEncoder().encode(runtimeConfig),
        { mode: "r" },
    );
    args.filesystem.mountFile(
        "/User/core-options.cfg",
        new TextEncoder().encode([
            `pcsx2_bios = ${quoted(biosName)}`,
            `pcsx2_renderer = "D3D11"`,
            `pcsx2_fastboot = "enabled"`,
            `pcsx2_shared_memory_cards = "enabled"`,
            "",
        ].join("\n")),
        { mode: "r" },
    );

    const options = args.configuration.options();
    options.core.placeholder = false;
    for (const [name, bytes] of Object.entries(args.configuration.render(args.gamepadBindings))) {
        args.filesystem.mountFile(`/Generated/${name}`, bytes, { mode: "rw" });
    }

    return {
        command: args.filesystem.path("/RetroArch/retroarch.exe"),
        argv: [
            "-c", args.filesystem.path("/User/retroarch.cfg"),
            "-L", args.filesystem.path("/RetroArch/cores/pcsx2_libretro.dll"),
            "--verbose",
            "--log-file", args.filesystem.path("/User/logs/lrps2.log"),
            "-f",
            args.filesystem.path(`/Game/${gameName}`),
        ],
        isolate: true,
        hooks: ["d3d11"],
        surfaces: [
            { index: 0, pattern: "RetroArch" },
        ],
    };
};

export default { setupLaunch };
