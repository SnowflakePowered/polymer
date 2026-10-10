import runtime from "pkg:bin/win-x64";
import { caches } from "polymer:cache";
import type {
    Actions, DirectoryResource, EmulatorLaunchArgs, FileEntry, PauseResumeContext, QuitContext, StateContext,
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

export const onLaunch = (args: EmulatorLaunchArgs<{ general: { FullscreenOnDoubleclick: boolean, StateSlot: number } }>) => {
    const fs = args.filesystem;
    fs.mount("/PPSSPP", runtime, { mode: "rx" });
    fs.mount("/PPSSPP/memstick");
    fs.mount("/PPSSPP/memstick/PSP/SAVEDATA", caches.saves, { mode: "rw" });
    fs.mount("/PPSSPP/memstick/PSP/PPSSPP_STATE", args.saveStates.directory, { mode: "rwx" });
    fs.mount("/User");
    const game = fs.mount("/Game", args.launchSet[0].package, { mode: "rx" });
    const options = args.configuration.options().general;
    options.FullscreenOnDoubleclick = false;
    options.StateSlot = 0;
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
        hooks: { graphics: ["d3d11"], input: ["xinput"] },
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

export default { onLaunch, onParseHotkey, onPauseResume, onStateSave, onStateResume, onQuit };
