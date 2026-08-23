// Host-backed view of snowglass-config's ConfigurationInstance. This object is
// deliberately opaque: the plugin may inspect and adjust option values, but it
// returns the same object to pluginhost, which alone drives serialization.
type Configuration = {
    options(): {
        main: {
            core: {
                gfx_backend: string,
                gci_folder_a_path: string,
                slot_a: string,
                slot_b: string,
            },
            input: {
                background_input: boolean,
            },
            interface: {
                skip_n_kit_warning: boolean,
            },
        },
    },
}

type Resource = Readonly<{ kind: "tree" | "file", path?: string }>;

type Filesystem = {
    mountHostFile(target: string, source: string, options: { mode: string }): void,
    mountTree(target: string, source: Resource | URL | string, options: { mode: string }): void,
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
    storage: Readonly<{ slotA?: Resource, slotB?: Resource }>,
}

function basename(path: string): string {
    const name = path.replaceAll("\\", "/").split("/").at(-1);
    if (!name) throw new TypeError(`path has no filename: ${path}`);
    return name;
}

export const setupLaunch = (args: SetupLaunchArgs) => {
    const gameName = basename(args.launchPath);

    args.filesystem.mountTree("/Dolphin", args.resources.runtime, { mode: "rx" });
    args.filesystem.mountScratch("/User", { mode: "rwx", backing: "temporary" });
    args.filesystem.mountScratch("/MemoryCard", { mode: "rwx", backing: "temporary" });
    if (args.storage.slotA) {
        args.filesystem.mountStorage("/MemoryCard/USA", args.storage.slotA, {
            mode: "rwx",
            format: "dolphin",
        });
    }
    args.filesystem.mountHostFile(`/Game/${gameName}`, args.launchPath, { mode: "r" });
    args.filesystem.setWorkingDirectory("/Dolphin");
    args.filesystem.setProfile({
        home: "/User",
        roamingAppData: "/User/AppData/Roaming",
        localAppData: "/User/AppData/Local",
        temp: "/User/Temp",
    });

    const options = args.configuration.options().main;
    options.core.gfx_backend = "D3D";
    options.core.gci_folder_a_path = args.filesystem.path("/MemoryCard/USA");
    options.core.slot_a = "MemoryCardFolder";
    options.core.slot_b = "None";
    // Snowglass owns the focused host window while Dolphin renders through an
    // isolated capture surface. Dolphin must continue polling the injected
    // virtual controller while its own hidden window is not foreground.
    options.input.background_input = true;
    // NKit images trigger a modal warning before emulation starts. Snowglass
    // launches Dolphin unattended, so the warning must never block startup.
    options.interface.skip_n_kit_warning = true;

    return {
        command: args.filesystem.path("/Dolphin/Dolphin.exe"),
        argv: [
            "-u", args.filesystem.path("/User"),
            "-b", "-e", args.filesystem.path(`/Game/${gameName}`),
        ],
        configRoot: args.filesystem.path("/User"),
        isolate: false,
        hooks: ["d3d11"],
        graphTracking: true,
        config: args.configuration,
        surfaces: [
            { index: 0, pattern: "Dolphin" },
        ]
    }
}

export default {
    setupLaunch
}
