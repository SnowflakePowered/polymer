import config from "pkg:plugin.toml";

type Configuration = {
    options(): {
        main: {
            settings_version: number,
            setup_wizard_incomplete: boolean,
            confirm_power_off: boolean,
            save_state_on_exit: boolean,
            pause_on_focus_loss: boolean,
        },
        bios: {
            search_directory: string,
            path_ntscu: string,
            patch_fast_boot: boolean,
        },
        folders: {
            cache: string,
            cheats: string,
            game_settings: string,
            input_profiles: string,
            patches: string,
            save_states: string,
            screenshots: string,
            textures: string,
            user_resources: string,
        },
        memorycards: {
            card1_type: string,
            card1_path: string,
            card2_type: string,
            directory: string,
            fast_forward_access: boolean,
        },
        logging: {
            log_level: string,
            log_to_console: boolean,
            log_to_file: boolean,
            memory_card: boolean,
            system: boolean,
        },
        input_sources: { sdl: boolean },
    },
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
    mountStorage(target: string, source: Resource, options: { mode: string, format?: string }): void,
    mountScratch(target: string, options: { mode: string, backing: "memory" | "temporary" }): void,
    path(path: string): string,
    setWorkingDirectory(path: string): void,
    setProfile(profile: { home?: string, roamingAppData?: string, localAppData?: string, temp?: string }): void,
};
type SetupLaunchArgs = {
    launchPath: string,
    console: string,
    portAssignments: { [group: string]: string[] },
    gamepadBindings: readonly GamepadBinding[],
    configuration: Configuration,
    filesystem: Filesystem,
    resources: { runtime: Resource },
    storage: Readonly<{ card1?: Resource, card2?: Resource }>,
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
    if (args.storage.card1) {
        args.filesystem.mountStorage("/User/memcards/snowglass.mcd", args.storage.card1, {
            mode: "rw",
            format: "raw",
        });
    } else {
        args.filesystem.mountScratch("/User/memcards", { mode: "rwx", backing: "temporary" });
    }
    args.filesystem.mountHostFile(`/User/bios/${biosName}`, config.plugin.config.bios, { mode: "r" });
    args.filesystem.mountHostTree("/Game", dirname(args.launchPath), { mode: "rx" });
    args.filesystem.setWorkingDirectory("/DuckStation");
    args.filesystem.setProfile({
        home: "/User",
        roamingAppData: "/User/AppData/Roaming",
        localAppData: "/User/AppData/Local",
        temp: "/User/Temp",
    });

    const options = args.configuration.options();
    options.main.settings_version = 3;
    options.main.setup_wizard_incomplete = false;
    options.main.confirm_power_off = false;
    options.main.save_state_on_exit = false;
    options.main.pause_on_focus_loss = false;
    options.bios.search_directory = path("/User/bios");
    options.bios.path_ntscu = biosName;
    options.bios.patch_fast_boot = true;
    options.folders.cache = path("/User/cache");
    options.folders.cheats = path("/User/cheats");
    options.folders.game_settings = path("/User/gamesettings");
    options.folders.input_profiles = path("/User/inputprofiles");
    options.folders.patches = path("/User/patches");
    options.folders.save_states = path("/User/savestates");
    options.folders.screenshots = path("/User/screenshots");
    options.folders.textures = path("/User/textures");
    options.folders.user_resources = path("/DuckStation/resources");
    options.memorycards.card1_type = args.storage.card1 ? "Shared" : "None";
    options.memorycards.card1_path = "snowglass.mcd";
    options.memorycards.card2_type = "None";
    options.memorycards.directory = path("/User/memcards");
    options.memorycards.fast_forward_access = false;
    options.logging.log_level = "Info";
    options.logging.log_to_console = false;
    options.logging.log_to_file = true;
    options.logging.memory_card = true;
    options.logging.system = true;
    options.input_sources.sdl = true;
    const settings = args.configuration.render(args.gamepadBindings)["settings.ini"];
    if (!settings) throw new TypeError("DuckStation configuration did not render settings.ini");
    args.filesystem.mountFile("/DuckStation/settings.ini", settings, { mode: "rw" });
    return {
        command: path("/DuckStation/duckstation-qt-x64-ReleaseLTCG.exe"),
        argv: ["-batch", "-fastboot", "--", path(`/Game/${gameName}`)],
        isolate: true,
        hooks: ["d3d11"],
        surfaces: [{ index: 0, pattern: "DuckStation|Crash Bandicoot" }],
    };
};

export default { setupLaunch };
