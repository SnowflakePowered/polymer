import { caches, config } from "polymer:plugin";
import { connect } from "polymer:database";
import { download } from "polymer:download";
import { Job } from "polymer:jobs";
import { graphicsApis, osIdentifier } from "polymer:sys";
import { unarchive } from "polymer:unarchive";
import { EmulatorUpdates } from "jsr:@polymer-fe/emulator-updates@^0.1.0";
import type {
    Actions, ConfigurationResult, DirectoryResource, EmulatorLaunchArgs, FileEntry, GraphicsHook, Package, PauseResumeContext,
    QuitContext, StateContext, UserConfiguration,
} from "polymer:api";
import { HotkeyParser } from "pkg:hotkeys.ts";

const SLOT_COUNT = 11;
const QUICK_RESUME_SLOT = 10;

class StateSlots {
    static current = 0;
    static paused = false;

    static async save(actions: Actions, directory: DirectoryResource, slot: number): Promise<FileEntry> {
        await StateSlots.select(actions, slot);
        await StateSlots.remove(directory, slot);
        StateSlots.activate(actions, "save_state");
        return await StateSlots.await(directory, slot);
    }

    static async load(actions: Actions, slot: number): Promise<void> {
        await StateSlots.select(actions, slot);
        StateSlots.activate(actions, "load_state");
    }

    static async select(actions: Actions, slot: number): Promise<void> {
        if (!Number.isInteger(slot) || slot < 0 || slot >= SLOT_COUNT) {
            throw new RangeError(`PPSSPP save-state slot ${slot} is outside the supported range`);
        }
        for (; StateSlots.current !== slot; StateSlots.current = (StateSlots.current + 1) % SLOT_COUNT) {
            StateSlots.activate(actions, "next_slot");
            await new Promise(resolve => setTimeout(resolve, 50));
        }
    }

    static activate(actions: Actions, name: string): void {
        const action = actions[name];
        if (!action) throw new Error(`PPSSPP action '${name}' is unavailable`);
        action.activate();
    }

    static async remove(directory: DirectoryResource, slot: number): Promise<void> {
        for await (const entry of directory) {
            if (entry.kind === "file" && entry.name.toLowerCase().endsWith(`_${slot}.ppst`)) {
                await directory.remove(entry.name);
            }
        }
    }

    static async await(directory: DirectoryResource, slot: number): Promise<FileEntry> {
        const deadline = Date.now() + 25_000;
        do {
            for await (const entry of directory) {
                if (entry.kind === "file" && entry.name.toLowerCase().endsWith(`_${slot}.ppst`)) return entry;
            }
            await new Promise(resolve => setTimeout(resolve, 50));
        } while (Date.now() < deadline);
        throw new Error(`PPSSPP did not create save-state slot ${slot}`);
    }
}

const runtime = new EmulatorUpdates({
    name: "PPSSPP",
    cache: caches.runtime,
    osIdentifier,
    channel: String(config.channel ?? "stable"),
    connect,
    download,
    unarchive,
    Job,
});

export const onPluginLoad = () => runtime.ready();

export const onUpdateContents = async () => runtime.update([await runtime.release("ppsspp")]);

const GRAPHICS_APIS = ([
    { hook: "d3d11", label: "Direct3D 11", value: "DIRECT3D11" },
    { hook: "vulkan", label: "Vulkan", value: "VULKAN" },
    { hook: "opengl", label: "OpenGL", value: "OPENGL" },
] satisfies { hook: GraphicsHook, label: string, value: string }[]).filter(api => graphicsApis.includes(api.hook));

const GRAPHICS_API_OPTION = {
    key: "api",
    title: "Graphics API",
    type: "select",
    default: GRAPHICS_APIS[0]?.hook ?? null,
    choices: GRAPHICS_APIS.map(api => ({ key: api.hook, label: api.label })),
} as const;

function graphicsApi(configuration?: UserConfiguration) {
    return GRAPHICS_APIS.find(api => api.hook === configuration?.graphics?.api) ?? GRAPHICS_APIS[0];
}

const MAX_CPU_CLOCK = 400;

const CPU_CLOCK_OPTION = {
    key: "clock",
    title: "CPU Clock",
    description: "Percentage of the console's native CPU speed. Higher values can reduce slowdown but may break some games.",
    type: "int",
    default: 100,
    min: 100,
    max: MAX_CPU_CLOCK,
} as const;

function cpuClock(configuration?: UserConfiguration): number {
    const value = configuration?.cpu?.clock;
    return typeof value === "number" && Number.isInteger(value) ? Math.min(MAX_CPU_CLOCK, Math.max(100, value)) : 100;
}

export const onConfigure = (_pkg: Package, userConfiguration?: UserConfiguration): ConfigurationResult => ({
    schema: "ppsspp/v1",
    groups: [
        ...(GRAPHICS_APIS.length > 1 ? [{ key: "graphics", title: "Graphics", options: [GRAPHICS_API_OPTION] }] : []),
        { key: "cpu", title: "CPU", options: [CPU_CLOCK_OPTION] },
    ],
    values: { graphics: { api: graphicsApi(userConfiguration)?.hook ?? null }, cpu: { clock: cpuClock(userConfiguration) } },
});

export const onLaunch = async (args: EmulatorLaunchArgs<{
    general: { FullscreenOnDoubleclick: boolean, StateSlot: number },
    graphics: { GraphicsBackend: string, DisabledGraphicsBackends: string },
    cpu: { CPUSpeed: number },
}>) => {
    const fs = args.filesystem;
    fs.mount("/PPSSPP", await runtime.launch(), { mode: "rx" });
    fs.mount("/PPSSPP/memstick");
    fs.mount("/PPSSPP/memstick/PSP/SAVEDATA", caches.saves, { mode: "rw" });
    fs.mount("/PPSSPP/memstick/PSP/PPSSPP_STATE", args.saveStates.directory, { mode: "rwx" });
    fs.mount("/User");
    const game = fs.mount("/Game", args.launchSet[0].package, { mode: "rx" });
    const options = args.configuration.options().general;
    options.FullscreenOnDoubleclick = false;
    options.StateSlot = 0;
    const api = graphicsApi(args.userConfiguration);
    args.configuration.options().graphics.GraphicsBackend = api.value;
    args.configuration.options().graphics.DisabledGraphicsBackends = api.hook === "vulkan" ? "" : "VULKAN";
    args.configuration.options().cpu.CPUSpeed = cpuClock(args.userConfiguration) === 100
        ? 0
        : Math.round(2.22 * cpuClock(args.userConfiguration));
    StateSlots.current = 0;
    StateSlots.paused = false;
    for (const [name, bytes] of Object.entries(args.configuration.render())) {
        fs.mount(`/PPSSPP/memstick/PSP/SYSTEM/${name}`, bytes, { mode: "rw" });
    }
    const quickResume = args.saveStates.quickResume;
    const argv = [game];
    if (quickResume) {
        argv.push(`--state=${fs.mount("/QuickResume/state.ppst", quickResume.state, { mode: "r" })}`);
    }

    return {
        command: fs.path("/PPSSPP/PPSSPPWindows64.exe"),
        workingDirectory: fs.path("/PPSSPP"),
        argv,
        env: [["USERPROFILE", fs.path("/User")], ["APPDATA", fs.path("/User/AppData/Roaming")],
            ["LOCALAPPDATA", fs.path("/User/AppData/Local")], ["TEMP", fs.path("/User/Temp")],
            ["TMP", fs.path("/User/Temp")]],
        isolate: true,
        hooks: { graphics: [api.hook], input: ["xinput"] },
        surfaces: [{ index: 0, pattern: "^PPSSPP" }],
        quickResume,
    };
};

export const onParseHotkey = (expression: string) => HotkeyParser.parse(expression);

export const onPauseResume = (context: PauseResumeContext) => {
    const paused = context.pauseState === "pause";
    if (paused === StateSlots.paused) return;
    StateSlots.activate(context.actions, "pause");
    StateSlots.paused = paused;
};

export const onStateSave = async (context: StateContext) => {
    await StateSlots.save(context.actions, context.directory, context.slot);
};

export const onStateResume = (context: StateContext) => StateSlots.load(context.actions, context.slot);

export const onQuit = (context: QuitContext) => StateSlots.save(context.actions, context.directory, QUICK_RESUME_SLOT);

export default { onPluginLoad, onUpdateContents, onConfigure, onLaunch, onParseHotkey, onPauseResume, onStateSave, onStateResume, onQuit };
