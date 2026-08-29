// Host-backed view of snowglass-config's ConfigurationInstance. This object is
// deliberately opaque: the plugin may inspect and adjust option values, but it
// installs the rendered files explicitly into the launch filesystem.
type Configuration = {
    options(): {
        main: {
            analytics: {
                enabled: boolean,
                permission_asked: boolean,
            },
            core: {
                gfx_backend: string,
                gci_folder_a_path: string,
                slot_a: string,
                slot_b: string,
            },
            general: {
                nand_root_path: string,
            },
            input: {
                background_input: boolean,
            },
            interface: {
                skip_n_kit_warning: boolean,
            },
        },
        ports: {
            NINTENDO_WII: {
                wii: Record<string, {
                    relative_input: boolean,
                }>,
            },
        },
    },
    render(bindings: readonly GamepadBinding[]): Readonly<Record<string, Uint8Array>>,
}

type GamepadBinding = Readonly<{
    target: Readonly<{ group: string, port: number }>,
    gamepad: number,
}>;

type Resource = Readonly<{ kind: "tree" | "file", path?: string }>;

type Package = Readonly<{
    titleId: string | null,
}>;

type ControllerMapping = Readonly<{
    group: string | null,
    port: number | null,
    layout: string,
    mouse: boolean,
    mapping: Readonly<Record<string, string>>,
}>;

type Filesystem = {
    mountHostFile(target: string, source: string, options: { mode: string }): void,
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
    package: Package,
    portAssignments: { [group: string]: string[] },
    controllerMappings: readonly ControllerMapping[],
    gamepadBindings: readonly GamepadBinding[],
    configuration: Configuration,
    filesystem: Filesystem,
    resources: { runtime: Resource },
    storage: Readonly<{ nand?: Resource, slotA?: Resource, slotB?: Resource }>,
}

function basename(path: string): string {
    const name = path.replaceAll("\\", "/").split("/").at(-1);
    if (!name) throw new TypeError(`path has no filename: ${path}`);
    return name;
}

const DISC_IMAGE_EXTENSIONS = [".iso", ".rvz", ".wbfs"];

function isDiscImage(path: string): boolean {
    const lowerPath = path.toLowerCase();
    return DISC_IMAGE_EXTENSIONS.some(extension => lowerPath.endsWith(extension));
}

const WII_POINTER_AXES = [
    ["pointer.x-", /^(?:left|right)_x$/],
    ["pointer.x+", /^(?:left|right)_x$/],
    ["pointer.y-", /^(?:left|right)_y$/],
    ["pointer.y+", /^(?:left|right)_y$/],
] as const;

function hasAnalogPointer(mapping: ControllerMapping): boolean {
    if (mapping.mouse) return false;
    return WII_POINTER_AXES.every(([target, expectedAxis]) => {
        const source = mapping.mapping[target]?.trim().toLowerCase();
        if (!source?.startsWith("axis:")) return false;
        return expectedAxis.test(source.slice("axis:".length).replace(/^[+-]/, ""));
    });
}

export const setupLaunch = (args: SetupLaunchArgs) => {
    const gameName = basename(args.launchPath);
    const isWii = args.console === "NINTENDO_WII";
    // Wii WADs are installed into the selected NAND before setupLaunch and
    // must continue to boot by title ID. Disc images instead need to remain
    // visible as files and be passed to Dolphin with -e.
    const launchesNandTitle = isWii && !isDiscImage(gameName);

    args.filesystem.mountTree("/Dolphin", args.resources.runtime, { mode: "rx" });
    args.filesystem.mountScratch("/User", { mode: "rwx", backing: "temporary" });
    args.filesystem.mountFile(
        "/User/Config/Logger.ini",
        new TextEncoder().encode([
            "[Logs]",
            "BOOT = True",
            "COMMON = True",
            "CORE = True",
            "IOS = True",
            "IOS_ES = True",
            "IOS_FS = True",
            "WII_IPC = True",
            "[Options]",
            "Verbosity = 4",
            "WriteToConsole = True",
            "WriteToFile = True",
            "WriteToWindow = False",
            "",
        ].join("\n")),
        { mode: "rw" },
    );
    args.filesystem.mountScratch("/User/Logs", { mode: "rwx", backing: "memory" });
    args.filesystem.mountFile("/User/Logs/dolphin.log", new Uint8Array(), { mode: "rw" });
    args.filesystem.mountScratch("/MemoryCard", { mode: "rwx", backing: "temporary" });
    if (args.storage.slotA) {
        args.filesystem.mountStorage("/MemoryCard/USA", args.storage.slotA, {
            mode: "rwx",
            format: "dolphin",
        });
    }
    if (isWii) {
        if (!args.storage.nand) {
            throw new TypeError("a Wii launch requires installed NAND storage");
        }
        args.filesystem.mountStorage("/User/Wii", args.storage.nand, { mode: "rwx" });
    }
    if (!launchesNandTitle) {
        args.filesystem.mountHostFile(`/Game/${gameName}`, args.launchPath, { mode: "r" });
    }
    args.filesystem.setWorkingDirectory("/Dolphin");
    args.filesystem.setProfile({
        home: "/User",
        roamingAppData: "/User/AppData/Roaming",
        localAppData: "/User/AppData/Local",
        temp: "/User/Temp",
    });

    const options = args.configuration.options().main;
    // Resolve Dolphin's first-run telemetry choice in generated Dolphin.ini.
    // Telemetry remains disabled and Dolphin does not open its consent dialog.
    options.analytics.enabled = false;
    options.analytics.permission_asked = true;
    options.core.gfx_backend = "D3D";
    options.core.gci_folder_a_path = args.filesystem.path("/MemoryCard/USA");
    options.core.slot_a = "MemoryCardFolder";
    options.core.slot_b = "None";
    if (isWii) {
        options.general.nand_root_path = args.filesystem.path("/User/Wii");
    }
    // Snowglass owns the focused host window while Dolphin renders through an
    // isolated capture surface. Dolphin must continue polling the injected
    // virtual controller while its own hidden window is not foreground.
    options.input.background_input = true;
    // NKit images trigger a modal warning before emulation starts. Snowglass
    // launches Dolphin unattended, so the warning must never block startup.
    options.interface.skip_n_kit_warning = true;

    if (isWii) {
        const wiiOptions = args.configuration.options().ports.NINTENDO_WII.wii;
        for (const mapping of args.controllerMappings) {
            if (mapping.group !== "wii" || mapping.port === null) continue;
            wiiOptions[mapping.port].relative_input = hasAnalogPointer(mapping);
        }
    }

    const argv = ["-u", args.filesystem.path("/User"), "-b"];
    if (launchesNandTitle) {
        const titleId = args.package.titleId?.replace(/^0x/i, "").toLowerCase();
        if (!titleId || !/^[0-9a-f]{16}$/.test(titleId)) {
            throw new TypeError("a Wii launch requires a 16-character installed title ID");
        }
        argv.push("--nand_title", titleId);
    } else {
        argv.push("-e", args.filesystem.path(`/Game/${gameName}`));
    }
    for (const [name, bytes] of Object.entries(args.configuration.render(args.gamepadBindings))) {
        args.filesystem.mountFile(`/User/${name}`, bytes, { mode: "rw" });
    }

    return {
        command: args.filesystem.path("/Dolphin/Dolphin.exe"),
        argv,
        isolate: false,
        hooks: ["d3d11"],
        graphTracking: true,
        surfaces: [
            { index: 0, pattern: "Dolphin" },
        ]
    }
}

type ControllerChangeContext = Readonly<{
    controllerMappings: readonly Readonly<{
        target: Readonly<{ group: string, port: number }>,
        gamepad: number,
        layout: string,
        connected: boolean,
    }>[],
    gamepads: readonly Readonly<{
        index: number,
        connected: boolean,
        connect(): void,
        disconnect(): void,
    }>[],
    configuration: Configuration,
    filesystem: Filesystem,
    flush(): void,
}>;

export const onControllerChanged = (context: ControllerChangeContext) => {
    const bindings = context.controllerMappings.map(mapping => ({
        target: mapping.target,
        gamepad: context.gamepads[mapping.gamepad],
    }));
    for (const mapping of context.controllerMappings) {
        const gamepad = context.gamepads[mapping.gamepad];
        if (!gamepad) throw new TypeError(`virtual gamepad ${mapping.gamepad} is unavailable`);
        if (mapping.connected) gamepad.connect();
        else gamepad.disconnect();
    }
    const wiiMappings = context.controllerMappings.filter(mapping => mapping.target.group === "wii");
    if (wiiMappings.length > 0) {
        const wiiOptions = context.configuration.options().ports.NINTENDO_WII.wii;
        for (const mapping of wiiMappings) {
            wiiOptions[mapping.target.port].relative_input = hasAnalogPointer(mapping);
        }
    }
    for (const [name, bytes] of Object.entries(context.configuration.render(bindings))) {
        context.filesystem.mountFile(`/User/${name}`, bytes, { mode: "rw" });
    }
};

export default {
    setupLaunch,
    onControllerChanged,
}
