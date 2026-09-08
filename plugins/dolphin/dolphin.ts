import runtime from "pkg:bin/win-x64";
import { uiAutomation } from "polymer:uiautomation";
import { actions } from "polymer:actions";
import type {
    Actions,
    Configuration as HostConfiguration,
    ControllerChangeContext as HostControllerChangeContext,
    ControllerMapping,
    ControllerTopology,
    ConfigurationResult,
    Package,
    DirectoryResource,
    FileEntry,
    Filesystem,
    QuitContext,
    Resource,
    PauseResumeContext,
    StateContext,
    UiNodeSnapshot,
    UserConfiguration,
} from "polymer:api";
import { HotkeyParser } from "pkg:hotkeys.ts";

type Configuration = HostConfiguration<{
        main: {
            analytics: {
                enabled: boolean,
                permission_asked: boolean,
            },
            core: {
                gfx_backend: string,
                gci_folder_a_path: string,
                enable_save_states: boolean,
                slot_a: string,
                slot_b: string,
                wiimote_continuous_scanning: boolean,
                wiimote_controller_interface: boolean,
            },
            general: {
                nand_root_path: string,
                hotkeys_require_focus: boolean,
            },
            display: {
                render_to_main: boolean,
                render_window_auto_size: boolean,
            },
            input: {
                background_input: boolean,
            },
            interface: {
                confirm_stop: boolean,
                skip_n_kit_warning: boolean,
            },
        },
        gfx: {
            settings: {
                aspect_ratio: string,
            },
        },
        ports: {
            NINTENDO_WII: {
                wii: Record<string, {
                    relative_input: boolean,
                    source: number,
                }>,
            },
        },
}>;

type EmulatorLaunchArgs = {
    launchPath: string,
    console: string,
    package: Package,
    controllerTopology: ControllerTopology,
    configuration: Configuration,
    filesystem: Filesystem,
    storage: Readonly<{ nand?: Resource, slotA?: Resource, slotB?: Resource }>,
    userConfiguration?: UserConfiguration,
    saveStates: Readonly<{
        directory: DirectoryResource,
        quickResume: Readonly<{ state: FileEntry, screenshot: FileEntry | null }> | null,
    }>,
}

function basename(path: string): string {
    const name = path.replaceAll("\\", "/").split("/").at(-1);
    if (!name) throw new TypeError(`path has no filename: ${path}`);
    return name;
}

const DISC_IMAGE_EXTENSIONS = [".iso", ".rvz", ".wbfs", ".gcz"];

function isDiscImage(path: string): boolean {
    const lowerPath = path.toLowerCase();
    return DISC_IMAGE_EXTENSIONS.some(extension => lowerPath.endsWith(extension));
}

function isNKit(path: string): boolean {
    return /\.nkit\.(?:iso|gcz)$/i.test(path);
}

function quickResumeEnabled(configuration?: UserConfiguration): boolean {
    return configuration?.polymer?.quickResume !== false;
}

export const onConfigure = (
    pkg: Package,
    userConfiguration?: UserConfiguration,
): ConfigurationResult => ({
    schema: "dolphin/v1",
    groups: [{
        key: "polymer",
        title: "Polymer",
        options: [{
            key: "quickResume",
            title: "Quick Resume",
            description: "Resume this game where you left off on its next launch.",
            type: "bool",
            default: true,
        }],
    }],
    values: {
        polymer: {
            quickResume: !isNKit(pkg.filename) && quickResumeEnabled(userConfiguration),
        },
    },
});

const WII_REMOTE_SLOT_COUNT = 4;
const WII_REMOTE_SOURCE_NAMES = ["None", "Emulated Wii Remote", "Real Wii Remote"] as const;
const DOLPHIN_WINDOW_CLOSE_SELECTOR = 'button[name="Close"][stable_id="Close"]';
let loadedWiimoteMappings: Array<ControllerMapping | undefined> | null = null;

function usesRealWiimote(mapping: ControllerMapping): boolean {
    if (!mapping.connected || mapping.target.group !== "wii") return false;
    const device = mapping.device;
    if (!device?.capabilities.includes("rawHid")) return false;
    return (device.vendorId === 0x057e
        && (device.productId === 0x0306 || device.productId === 0x0330))
        || device.profile?.startsWith("nintendo.wiimote.") === true
        || device.name.startsWith("Nintendo RVL-CNT");
}

function wiimoteSource(mapping: ControllerMapping): 0 | 1 | 2 {
    return usesRealWiimote(mapping) ? 2 : mapping.connected ? 1 : 0;
}

function usesRelativeInput(mapping: ControllerMapping): boolean {
    // This configures the persistent emulated mapping for the slot. Keep it
    // while the source is None so reconnecting a gamepad cannot reset it.
    return mapping.pointer.mode === "relative";
}

function checkedState(snapshot: UiNodeSnapshot): boolean {
    const checked = snapshot.states.checked;
    if (checked === true || checked === "On") return true;
    if (checked === false || checked === "Off") return false;
    throw new TypeError(`UIA checkbox reported invalid checked state ${JSON.stringify(checked)}`);
}

function requiresWiimoteMappingReload(
    loaded: ControllerMapping | undefined,
    current: ControllerMapping,
): boolean {
    if (!loaded) return true;
    // Dolphin only sees the stable Polymer virtual gamepad, its emulated
    // layout, and IR mode. Physical projection and custom bindings are applied
    // by the host before events reach that virtual device.
    return loaded.gamepad !== current.gamepad
        || loaded.layout !== current.layout
        || usesRelativeInput(loaded) !== usesRelativeInput(current);
}

function wiimoteMappings(topology: ControllerTopology): Array<ControllerMapping | undefined> {
    const slots = Array<ControllerMapping | undefined>(WII_REMOTE_SLOT_COUNT).fill(undefined);
    for (const mapping of topology.mappings.filter(mapping => mapping.target.group === "wii")) {
        const port = mapping.target.port;
        if (!Number.isInteger(port) || port < 0 || port >= WII_REMOTE_SLOT_COUNT) {
            throw new RangeError(`invalid Wii Remote port ${port}`);
        }
        if (slots[port]) throw new TypeError(`Wii Remote port ${port} is mapped more than once`);
        slots[port] = mapping;
    }
    return slots;
}

function wiimoteDiagnostics(topology: ControllerTopology) {
    return wiimoteMappings(topology).map((mapping, port) => mapping
        ? {
            port,
            gamepad: mapping.gamepad,
            connected: mapping.connected,
            requestedSource: WII_REMOTE_SOURCE_NAMES[wiimoteSource(mapping)],
            relativeInput: usesRelativeInput(mapping),
            pointer: mapping.pointer,
            source: mapping.source ?? null,
            device: mapping.device
                ? {
                    id: mapping.device.id,
                    name: mapping.device.name,
                    profile: mapping.device.profile,
                    vendorId: mapping.device.vendorId,
                    productId: mapping.device.productId,
                    role: mapping.device.role,
                    capabilities: mapping.device.capabilities,
                }
                : null,
        }
        : {
            port,
            gamepad: null,
            connected: false,
            requestedSource: "None",
            relativeInput: false,
            pointer: null,
            source: null,
            device: null,
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

async function applyWiiConnectionChanges(
    context: ControllerChangeContext,
    topology: ControllerTopology,
): Promise<void> {
    const automationStarted = Date.now();
    const mappings = wiimoteMappings(topology);
    const previousMappings = wiimoteMappings(pointerTopology(context.previousTopology));
    if (mappings.every(mapping => mapping === undefined)) return;
    loadedWiimoteMappings ??= [...previousMappings];
    const current = mappings
        .map(mapping => mapping ? wiimoteSource(mapping) : 0);
    const previousSources = previousMappings
        .map(mapping => mapping ? wiimoteSource(mapping) : 0);
    const mappingReloadPorts = new Set(mappings.flatMap((mapping, port) =>
        mapping
            && current[port] === 1
            && requiresWiimoteMappingReload(loadedWiimoteMappings?.[port], mapping)
            ? [port]
            : [],
    ));
    console.info(`polymer dolphin: controller topology previous=${JSON.stringify(wiimoteDiagnostics(context.previousTopology))} requested=${JSON.stringify(wiimoteDiagnostics(topology))}`);
    if (current.every((source, port) => source === previousSources[port])
        && mappingReloadPorts.size === 0) {
        console.info("polymer dolphin: UIA controller configuration skipped; live sources and mappings are unchanged");
        return;
    }

    // The control window title is Dolphin's source revision. The render window
    // appends pipe-delimited CPU, graphics, audio, and optional game details.
    const dolphin = await uiAutomation.waitForWindow({
        pattern: "^Dolphin [^|]+$",
        selector: 'button[name="Controllers"]',
    });
    await dolphin.locator('button[name="Controllers"]').nth(1).press();

    const settings = await uiAutomation.waitForWindow({
        pattern: "^Settings$",
        selector: 'group[name="Wii Remotes"] combo_box[enabled="true"]',
    });
    const settingsReadyAt = Date.now();
    try {
        const sources = settings.locator(
            'group[name="Wii Remotes"] combo_box[enabled="true"]',
        );
        const before = (await sources.all({ limit: WII_REMOTE_SLOT_COUNT }))
            .map((snapshot, port) => ({
                port,
                name: snapshot.name,
                value: snapshot.value,
                stableId: snapshot.stableId,
            }));
        if (before.length < WII_REMOTE_SLOT_COUNT) {
            throw new RangeError(`Dolphin exposed ${before.length} enabled Wii Remote source controls, expected at least ${WII_REMOTE_SLOT_COUNT}`);
        }
        console.info(`polymer dolphin: UIA Wii sources before=${JSON.stringify(before)} desired=${JSON.stringify(current.map(source => WII_REMOTE_SOURCE_NAMES[source]))}`);
        const sourcesStarted = Date.now();
        const observed = before.map(snapshot => snapshot.value ?? snapshot.name);
        // Arm destination real slots before releasing their old slots. Dolphin
        // moves a released remote through a pool that expires after five
        // seconds; arming first lets HandleWiimoteSourceChange transfer it
        // immediately instead of leaving it in the pool while UIA configures
        // an emulated slot.
        for (let port = 0; port < WII_REMOTE_SLOT_COUNT; port++) {
            if (current[port] === 2 && observed[port] !== "Real Wii Remote") {
                const previousSource = observed[port];
                const sourceStarted = Date.now();
                await sources.nth(port + 1).selectOption("Real Wii Remote");
                observed[port] = "Real Wii Remote";
                console.info(`polymer dolphin: selected Wii Remote ${port + 1} source ${JSON.stringify(previousSource)}->"Real Wii Remote" elapsed_ms=${Date.now() - sourceStarted}`);
            }
        }
        for (const [port, source] of current.entries()) {
            const desired = WII_REMOTE_SOURCE_NAMES[source];
            if (observed[port] !== desired) {
                const previousSource = observed[port];
                const sourceStarted = Date.now();
                await sources.nth(port + 1).selectOption(desired);
                observed[port] = desired;
                console.info(`polymer dolphin: selected Wii Remote ${port + 1} source ${JSON.stringify(previousSource)}->${JSON.stringify(desired)} elapsed_ms=${Date.now() - sourceStarted}`);
            }
        }
        const sourcesAppliedAt = Date.now();

        // Dolphin loads every emulated slot from WiimoteNew.ini at startup, so
        // None -> Emulated only needs the source combo. Reopen a mapping window
        // when the generated mapping itself changed; constructing these Qt
        // dialogs is by far the most expensive part of this callback.
        const configure = settings.locator(
            'group[name="Wii Remotes"] button[name="Configure"]',
        );
        const reloadedPorts: number[] = [];
        for (const [port, source] of current.entries()) {
            if (source !== 1) continue;
            const currentMapping = mappings[port];
            if (!currentMapping) continue;
            if (!mappingReloadPorts.has(port)) continue;
            const mappingStarted = Date.now();
            const pattern = `^Wii Remote ${port + 1}$`;
            const staleWindows = await uiAutomation.windows({ pattern });
            for (const stale of staleWindows) {
                await stale.locator(DOLPHIN_WINDOW_CLOSE_SELECTOR).nth(1).press();
            }
            if (staleWindows.length !== 0) {
                const cleanupStarted = Date.now();
                while ((await uiAutomation.windows({ pattern })).length !== 0) {
                    if (Date.now() - cleanupStarted >= 2_000) {
                        throw new Error(`Dolphin Wii Remote ${port + 1} configuration did not close`);
                    }
                    await new Promise(resolve => setTimeout(resolve, 25));
                }
            }
            await configure.nth(port + 1).press();
            const mapping = await uiAutomation.waitForWindow({
                pattern,
            });
            try {
                await mapping.locator('tab[name="Motion Simulation"]').nth(1).click();
                // Dolphin puts empty QCheckBoxes beside separate QFormLayout
                // labels. Qt UIA consequently names both Cursor bool settings
                // "Point". Cursor defines Relative Input before Auto-Hide, so
                // the first of the two Point checkboxes is deterministic.
                const pointSettings = mapping.locator('check_box[name="Point"]');
                const pointSettingSnapshots = await pointSettings.all({ limit: 3 });
                if (pointSettingSnapshots.length !== 2) {
                    throw new Error(`Dolphin Wii Remote ${port + 1} exposed ${pointSettingSnapshots.length} Point checkboxes, expected Relative Input and Auto-Hide`);
                }
                const relativeInput = pointSettings.nth(1);
                const desired = mappings[port] ? usesRelativeInput(mappings[port]) : false;
                let snapshot = pointSettingSnapshots[0];
                const previousRelativeInput = checkedState(snapshot);
                let observed = previousRelativeInput;
                if (observed !== desired) {
                    await relativeInput.toggle();
                    const toggleStarted = Date.now();
                    do {
                        snapshot = await relativeInput.snapshot();
                        observed = checkedState(snapshot);
                        if (observed === desired) break;
                        await new Promise(resolve => setTimeout(resolve, 25));
                    } while (Date.now() - toggleStarted < 1_000);
                }
                if (observed !== desired) {
                    throw new Error(`Dolphin Wii Remote ${port + 1} Relative Input is ${JSON.stringify(snapshot.states.checked)}, expected ${desired}`);
                }
                reloadedPorts.push(port + 1);
                console.info(`polymer dolphin: reloaded Wii Remote ${port + 1} mappings relativeInput=${JSON.stringify(previousRelativeInput)}->${desired} elapsed_ms=${Date.now() - mappingStarted}`);
            } finally {
                await mapping.locator(DOLPHIN_WINDOW_CLOSE_SELECTOR).nth(1).press();
            }
            loadedWiimoteMappings[port] = currentMapping;
        }
        const mappingWindowsAppliedAt = Date.now();
        const after = (await sources.all({ limit: WII_REMOTE_SLOT_COUNT }))
            .map((snapshot, port) => ({
                port,
                name: snapshot.name,
                value: snapshot.value,
                stableId: snapshot.stableId,
            }));
        if (after.length < WII_REMOTE_SLOT_COUNT) {
            throw new RangeError(`Dolphin exposed ${after.length} enabled Wii Remote source controls after applying changes, expected at least ${WII_REMOTE_SLOT_COUNT}`);
        }
        for (const [port, snapshot] of after.entries()) {
            const observed = snapshot.value ?? snapshot.name;
            if (observed !== WII_REMOTE_SOURCE_NAMES[current[port]]) {
                throw new Error(`Dolphin Wii Remote ${port + 1} source is ${JSON.stringify(observed)}, expected ${JSON.stringify(WII_REMOTE_SOURCE_NAMES[current[port]])}`);
            }
        }
        console.info(`polymer dolphin: UIA Wii sources applied=${JSON.stringify(after)}`);
        console.info(`polymer dolphin: UIA controller configuration timings=${JSON.stringify({
            settingsReadyMs: settingsReadyAt - automationStarted,
            sourcesMs: sourcesAppliedAt - sourcesStarted,
            mappingWindowsMs: mappingWindowsAppliedAt - sourcesAppliedAt,
            verificationMs: Date.now() - mappingWindowsAppliedAt,
            reloadedPorts,
        })}`);
    } finally {
        await settings.locator(DOLPHIN_WINDOW_CLOSE_SELECTOR).nth(1).press();
    }
}

export const onLaunch = (args: EmulatorLaunchArgs) => {
    const gameName = basename(args.launchPath);
    const isWii = args.console === "NINTENDO_WII";
    const controllerTopology = pointerTopology(args.controllerTopology);
    // Wii WADs are installed into the selected NAND before onLaunch and
    // must continue to boot by title ID. Disc images instead need to remain
    // visible as files and be passed to Dolphin with -e.
    const launchesNandTitle = isWii && !isDiscImage(gameName);

    args.filesystem.mountTree("/Dolphin", runtime, { mode: "rx" });
    args.filesystem.mountScratch("/User", { mode: "rwx", backing: "temporary" });
    args.filesystem.mountTree("/User/StateSaves", args.saveStates.directory, { mode: "rwx" });
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
            "WIIMOTE = True",
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
    options.core.gfx_backend = "D3D12";
    args.configuration.options().gfx.settings.aspect_ratio = "Stretch";
    options.core.gci_folder_a_path = args.filesystem.path("/MemoryCard/USA");
    // Dolphin disables save states by default in current development builds.
    // Polymer uses them for both explicit state slots and quick resume.
    options.core.enable_save_states = true;
    options.core.slot_a = "MemoryCardFolder";
    options.core.slot_b = "None";

    options.general.hotkeys_require_focus = false;
    // Keep Dolphin's control window available for live settings automation;
    // the D3D render surface remains a separate tracked window.
    options.display.render_to_main = false;
    // The fixed swapchain owns the render size; XFB-driven Qt resizes can
    // otherwise replace the client dimensions after Polymer sets them.
    options.display.render_window_auto_size = false;
    if (isWii) {
        options.general.nand_root_path = args.filesystem.path("/User/Wii");
    }
    // Polymer owns the focused host window while Dolphin renders through an
    // isolated capture surface. Dolphin must continue polling the injected
    // virtual controller while its own hidden window is not foreground.
    options.input.background_input = true;
    // NKit images trigger a modal warning before emulation starts. Polymer
    // launches Dolphin unattended, so the warning must never block startup.
    options.interface.confirm_stop = false;
    options.interface.skip_n_kit_warning = true;

    if (isWii) {
        console.info(`polymer dolphin: launch Wii assignments=${JSON.stringify(wiimoteDiagnostics(controllerTopology))}`);
        // The synthetic HID topology can arrive just after Dolphin's first
        // scanner pass and can change while a mixed-controller session is live.
        // Keep the Real Wiimote scanner active, but do not expose its devices
        // through ControllerInterface where the SDL/emulated path could claim it.
        options.core.wiimote_continuous_scanning = true;
        options.core.wiimote_controller_interface = false;
        const wiiOptions = args.configuration.options().ports.NINTENDO_WII.wii;
        for (const [port, mapping] of wiimoteMappings(controllerTopology).entries()) {
            const slot = wiiOptions[port];
            if (!slot) throw new TypeError(`configuration omits Wii Remote port ${port}`);
            slot.relative_input = mapping ? usesRelativeInput(mapping) : false;
            slot.source = mapping ? wiimoteSource(mapping) : 0;
        }
    }

    // Batch mode never shows Dolphin's control window. Polymer already
    // isolates that UI on a private desktop and owns process termination.
    const quickResume = quickResumeEnabled(args.userConfiguration) && !isNKit(args.launchPath)
        ? args.saveStates.quickResume
        : null;
    const argv = ["-u", args.filesystem.path("/User")];
    if (launchesNandTitle) {
        const titleId = args.package.titleId?.replace(/^0x/i, "").toLowerCase();
        if (!titleId || !/^[0-9a-f]{16}$/.test(titleId)) {
            throw new TypeError("a Wii launch requires a 16-character installed title ID");
        }
        argv.push("--nand_title", titleId);
    } else {
        argv.push("-e", args.filesystem.path(`/Game/${gameName}`));
    }
    if (quickResume) {
        args.filesystem.mountFile("/QuickResume/state.sav", quickResume.state, { mode: "r" });
        argv.push("--save_state", args.filesystem.path("/QuickResume/state.sav"));
    }
    args.filesystem.mountConfiguration("/User", args.configuration, { mode: "rw" });

    return {
        command: args.filesystem.path("/Dolphin/Dolphin.exe"),
        argv,
        isolate: false,
        hooks: {
            graphics: ["d3d12"],
            input: ["sdl3", "bluetooth-api-win32"],
        },
        controllerTopology,
        surfaces: [
            {
                index: 0,
                pattern: "Dolphin",
                resolution: isWii ? [854, 480] : [640, 528],
            },
        ],
        quickResume,
    }
}

type ControllerChangeContext = HostControllerChangeContext<ReturnType<Configuration["options"]>>;

export const onControllerChanged = async (context: ControllerChangeContext) => {
    const started = Date.now();
    const topology = pointerTopology(context.topology);
    const mappings = wiimoteMappings(topology);
    if (mappings.every(mapping => mapping === undefined)) {
        return { controllerTopology: topology };
    }
    const wiiOptions = context.configuration.options().ports.NINTENDO_WII.wii;
    for (const [port, mapping] of mappings.entries()) {
        const slot = wiiOptions[port];
        if (!slot) throw new TypeError(`configuration omits Wii Remote port ${port}`);
        slot.relative_input = mapping ? usesRelativeInput(mapping) : false;
        slot.source = mapping ? wiimoteSource(mapping) : 0;
    }
    context.filesystem.flush();
    console.info("polymer dolphin: published controller configuration before UI automation");
    await applyWiiConnectionChanges(context, topology);
    console.info(`polymer dolphin: onControllerChanged completed in ${Date.now() - started}ms applied=${JSON.stringify(wiimoteDiagnostics(topology))}`);
    return { controllerTopology: topology };
};

export const onParseHotkey = (expression: string) => HotkeyParser.dolphin(expression);

export const onPauseResume = (context: PauseResumeContext) => {
    actions.pause?.activate();
};

function nativeSlot(slot: number): number {
    if (!Number.isInteger(slot) || slot < 0 || slot >= 10) {
        throw new RangeError(`Dolphin save-state slot ${slot} is outside the supported range`);
    }
    return slot + 1;
}

async function removeNativeState(directory: DirectoryResource, slot: number): Promise<void> {
    const suffix = `.s${slot.toString().padStart(2, "0")}`;
    for await (const entry of directory) {
        if (entry.kind === "file" && entry.name.toLowerCase().endsWith(suffix)) {
            await directory.remove(entry.name);
        }
    }
}

async function awaitNativeState(directory: DirectoryResource, slot: number): Promise<FileEntry> {
    const suffix = `.s${slot.toString().padStart(2, "0")}`;
    const deadline = Date.now() + 25_000;
    do {
        for await (const entry of directory) {
            if (entry.kind === "file" && entry.name.toLowerCase().endsWith(suffix)) return entry;
        }
        await new Promise(resolve => setTimeout(resolve, 50));
    } while (Date.now() < deadline);
    throw new Error(`Dolphin did not create save-state slot ${slot}`);
}

function activateStateAction(actions: Actions, operation: "load" | "save", slot: number): void {
    const name = `${operation}_state_${slot}`;
    const action = actions[name];
    if (!action) throw new Error(`Dolphin action '${name}' is unavailable`);
    action.activate();
}

export const onStateSave = async (context: StateContext) => {
    const slot = nativeSlot(context.slot);
    await removeNativeState(context.directory, slot);
    activateStateAction(actions, "save", slot);
    await awaitNativeState(context.directory, slot);
}

export const onStateResume = (context: StateContext) => {
    const slot = nativeSlot(context.slot);
    activateStateAction(actions, "load", slot);
}

export const onQuit = async (context: QuitContext): Promise<FileEntry | null> => {
    if (!quickResumeEnabled(context.userConfiguration) || isNKit(context.launchPath)) return null;
    const slot = 10;
    await removeNativeState(context.directory, slot);
    activateStateAction(actions, "save", slot);
    return await awaitNativeState(context.directory, slot);
}

export default {
    onConfigure,
    onLaunch,
    onPauseResume,
    onStateSave,
    onStateResume,
    onQuit,
    onControllerChanged,
    onParseHotkey,
}
