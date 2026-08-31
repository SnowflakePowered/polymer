import type {
    Actions,
    Configuration as HostConfiguration,
    ControllerChangeContext as HostControllerChangeContext,
    ControllerMapping,
    ControllerTopology,
    Filesystem,
    InputProvider,
    Resource,
    PauseResumeContext,
} from "../plugin-api.ts";
import { HotkeyParser } from "./hotkeys.ts";

// Host-backed view of snowglass-config's ConfigurationInstance. This object is
// deliberately opaque: the plugin may inspect and adjust option values, but it
// installs the rendered files explicitly into the launch filesystem.
type Configuration = HostConfiguration<{
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
                wiimote_continuous_scanning: boolean,
                wiimote_controller_interface: boolean,
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
}>;

type Package = Readonly<{
    titleId: string | null,
}>;

type EmulatorLaunchArgs = {
    actions: Actions,
    launchPath: string,
    console: string,
    package: Package,
    controllerTopology: ControllerTopology,
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
    ["pointer.x-", /^analog\.stick\.(?:left|right)\.x[+-]$/],
    ["pointer.x+", /^analog\.stick\.(?:left|right)\.x[+-]$/],
    ["pointer.y-", /^analog\.stick\.(?:left|right)\.y[+-]$/],
    ["pointer.y+", /^analog\.stick\.(?:left|right)\.y[+-]$/],
] as const;

function hasAnalogPointer(mapping: Pick<ControllerMapping, "mouse" | "mapping">): boolean {
    if (mapping.mouse) return false;
    return WII_POINTER_AXES.every(([target, expectedAxis]) => {
        const source = mapping.mapping[target]?.trim().toLowerCase();
        return source !== undefined && expectedAxis.test(source);
    });
}

const WIIMOTE_CONFIGURATION = "Config/WiimoteNew.ini";

function rawWiimote(mapping: ControllerMapping): InputProvider | null {
    const device = mapping.device;
    if (!mapping.connected || !device?.id.startsWith("bt:") || mapping.target.group !== "wii") {
        return null;
    }
    const profile = device.profile ?? "";
    const recognized = profile.startsWith("nintendo.wiimote.")
        || device.name.startsWith("Nintendo RVL-CNT");
    if (!recognized || !device.capabilities.includes("rawHid")) return null;

    const reportedProduct = device.productId;
    const productId = typeof reportedProduct === "number"
        && (reportedProduct === 0x0306 || reportedProduct === 0x0330)
        ? reportedProduct
        : profile === "nintendo.wiimote.plus" || device.name.endsWith("-TR")
            ? 0x0330
            : 0x0306;
    return {
        type: "rawHid",
        sourceId: device.id,
        port: mapping.target.port,
        productId,
    };
}

function rawWiimoteProviders(topology: ControllerTopology): InputProvider[] {
    return topology.mappings.flatMap(mapping => {
        const provider = rawWiimote(mapping);
        return provider ? [provider] : [];
    });
}

function pointerTopology(topology: ControllerTopology): ControllerTopology {
    return {
        mappings: topology.mappings.map(mapping => ({
            ...mapping,
            pointer: {
                ...mapping.pointer,
                mode: mapping.target.group !== "wii"
                    ? "none"
                    : mapping.device?.capabilities.includes("pointerAngular")
                        ? "absoluteDevice"
                        : "relative",
            },
        })),
    };
}

function renderedConfiguration(
    configuration: Configuration,
    topology: ControllerTopology,
): Readonly<Record<string, Uint8Array>> {
    const files = configuration.render();
    const wiimoteConfiguration = files[WIIMOTE_CONFIGURATION];
    if (!wiimoteConfiguration) return files;

    const sources = new Map(
        topology.mappings
            .filter(mapping => mapping.target.group === "wii")
            .map(mapping => [
                mapping.target.port,
                rawWiimote(mapping) ? 2 : mapping.connected ? 1 : 0,
            ] as const),
    );
    let port: number | null = null;
    const text = new TextDecoder().decode(wiimoteConfiguration);
    const patched = text.split("\n").map(line => {
        const section = /^\[Wiimote(\d+)]\r?$/.exec(line);
        if (section) {
            port = Number(section[1]) - 1;
            return line;
        }
        if (/^\[.*]\r?$/.test(line)) {
            port = null;
            return line;
        }
        if (port === null || !/^Source=/.test(line)) return line;
        const ending = line.endsWith("\r") ? "\r" : "";
        return `Source=${sources.get(port) ?? 0}${ending}`;
    }).join("\n");

    return {
        ...files,
        [WIIMOTE_CONFIGURATION]: new TextEncoder().encode(patched),
    };
}

function mountConfiguration(
    filesystem: Filesystem,
    configuration: Configuration,
    topology: ControllerTopology,
): void {
    for (const [name, bytes] of Object.entries(renderedConfiguration(configuration, topology))) {
        filesystem.mountFile(`/User/${name}`, bytes, { mode: "rw" });
    }
}

async function applyWiiConnectionChanges(context: ControllerChangeContext): Promise<void> {
    const source = (mapping: ControllerMapping): number =>
        rawWiimote(mapping) ? 2 : mapping.connected ? 1 : 0;
    const previous = new Map(
        context.previousTopology.mappings
            .filter(mapping => mapping.target.group === "wii")
            .map(mapping => [mapping.target.port, source(mapping)] as const),
    );
    const changed = context.topology.mappings
        .filter(mapping =>
            mapping.target.group === "wii"
            && previous.get(mapping.target.port) !== source(mapping)
        )
        .sort((left, right) => left.target.port - right.target.port);
    if (changed.length === 0) return;

    const current = context.topology.mappings
        .filter(mapping => mapping.target.group === "wii")
        .sort((left, right) => left.target.port - right.target.port);

    const dolphin = await context.ui.waitForWindow({ title: { contains: "Dolphin" } });
    await dolphin.setLogicalFocus();
    await dolphin.keyboard.press("KeyO", { modifiers: ["Alt"] });
    await dolphin.keyboard.press("KeyC");

    const settings = await context.ui.waitForWindow({ title: { equals: "Settings" } });
    try {
        const sources = settings.locator('group[name="Wii Remotes"] combo_box');
        for (const mapping of context.previousTopology.mappings
            .filter(mapping => mapping.target.group === "wii" && source(mapping) === 2)
            .sort((left, right) => left.target.port - right.target.port)) {
            await sources.nth(mapping.target.port + 1).selectOption("None");
        }
        for (const mapping of current) {
            await sources
                .nth(mapping.target.port + 1)
                .selectOption(source(mapping) === 2
                    ? "Real Wii Remote"
                    : source(mapping) === 1
                        ? "Emulated Wii Remote"
                        : "None");
        }
    } finally {
        await settings.keyboard.press("Escape");
    }
}

export const onLaunch = (args: EmulatorLaunchArgs) => {
    const gameName = basename(args.launchPath);
    const isWii = args.console === "NINTENDO_WII";
    // Wii WADs are installed into the selected NAND before onLaunch and
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
        // The synthetic HID topology can arrive just after Dolphin's first
        // scanner pass and can change while a mixed-controller session is live.
        // Keep the Real Wiimote scanner active, but do not expose its devices
        // through ControllerInterface where the SDL/emulated path could claim it.
        options.core.wiimote_continuous_scanning = true;
        options.core.wiimote_controller_interface = false;
        const wiiOptions = args.configuration.options().ports.NINTENDO_WII.wii;
        for (const mapping of args.controllerTopology.mappings) {
            if (mapping.target.group !== "wii") continue;
            wiiOptions[mapping.target.port].relative_input = hasAnalogPointer(mapping);
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
    mountConfiguration(args.filesystem, args.configuration, args.controllerTopology);

    const controllerTopology = pointerTopology(args.controllerTopology);
    const inputProviders = rawWiimoteProviders(controllerTopology);
    return {
        command: args.filesystem.path("/Dolphin/Dolphin.exe"),
        argv,
        isolate: true,
        hooks: ["d3d11"],
        inputHooks: isWii ? ["bluetooth-api-win32"] : [],
        inputProviders,
        controllerTopology,
        graphTracking: true,
        surfaces: [
            { index: 0, pattern: "Dolphin" },
        ]
    }
}

type ControllerChangeContext = HostControllerChangeContext<ReturnType<Configuration["options"]>>;

export const onControllerChanged = async (context: ControllerChangeContext) => {
    const topology = pointerTopology(context.topology);
    const wiiMappings = topology.mappings.filter(mapping => mapping.target.group === "wii");
    if (wiiMappings.length > 0) {
        const wiiOptions = context.configuration.options().ports.NINTENDO_WII.wii;
        for (const mapping of wiiMappings) {
            wiiOptions[mapping.target.port].relative_input = hasAnalogPointer(mapping);
        }
    }
    mountConfiguration(context.filesystem, context.configuration, topology);
    await applyWiiConnectionChanges(context);
    return { inputProviders: rawWiimoteProviders(topology), controllerTopology: topology };
};

export const onParseHotkey = (expression: string) => HotkeyParser.dolphin(expression);

export const onPauseResume = (context: PauseResumeContext) => {
    context.actions.pause?.activate();
};

export default {
    onLaunch,
    onPauseResume,
    onControllerChanged,
    onParseHotkey,
}
