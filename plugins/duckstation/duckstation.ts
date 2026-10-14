import { caches, config } from "polymer:plugin";
import { connect } from "polymer:database";
import { download } from "polymer:download";
import { Job } from "polymer:jobs";
import { osIdentifier } from "polymer:sys";
import { unarchive } from "polymer:unarchive";
import { EmulatorUpdates } from "jsr:@polymer-fe/emulator-updates@^0.1.0";
import type { DiscChangeContext, EmulatorLaunchArgs, PauseResumeContext, Storage } from "polymer:api";
import { HotkeyParser } from "pkg:hotkeys.ts";

type Options = {
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
        gamelist: { recursive_paths: string },
        autoupdater: { check_at_startup: boolean },
};

let currentDisc = 0;

const runtime = new EmulatorUpdates({
    name: "DuckStation",
    cache: caches.runtime,
    osIdentifier,
    channel: String(config.channel ?? "stable"),
    connect,
    download,
    unarchive,
    Job,
});

export const onPluginLoad = () => runtime.ready();

export const onUpdateContents = async () => runtime.update([await runtime.release("duckstation")]);

export const onLaunch = async (args: EmulatorLaunchArgs<Options, Readonly<{ card1?: Storage, card2?: Storage }>>) => {
    const biosFiles = (await args.firmwareStore.list()).filter(entry => entry.platform === "SONY_PSX");
    if (biosFiles.length === 0) {
        throw new Error("DuckStation requires a PlayStation BIOS. Drag and drop a PlayStation BIOS file into Polymer, then launch again.");
    }
    const path = (virtualPath: string) => args.filesystem.path(virtualPath);

    args.filesystem.mount("/DuckStation", await runtime.launch(), { mode: "rx" });
    args.filesystem.mount("/DuckStation/portable.txt", new Uint8Array(), { mode: "r" });
    args.filesystem.mount("/DuckStation/duckstation.log", new Uint8Array(), { mode: "rw" });
    if (args.storage.card1) {
        args.filesystem.mount("/User/memcards/polymer.mcd", args.storage.card1, {
            mode: "rw",
            format: "raw",
        });
    } else {
        args.filesystem.mount("/User/memcards");
    }
    for (const [index, bios] of biosFiles.entries()) {
        const file = await args.firmwareStore.open(bios.label);
        args.filesystem.mount(`/User/bios/bios-${index}.bin`, file, { mode: "r" });
    }
    const discPaths = args.launchSet.length > 1
        ? args.launchSet.map((disc, index) => args.filesystem.mount(`/Game/Disc${index}`, disc.package, { mode: "rx" }))
        : [args.filesystem.mount("/Game", args.launchSet[0].package, { mode: "rx" })];
    currentDisc = args.initialDiscIndex;

    const options = args.configuration.options();
    options.main.settings_version = 3;
    options.main.setup_wizard_incomplete = false;
    options.main.confirm_power_off = false;
    options.main.save_state_on_exit = false;
    options.main.pause_on_focus_loss = false;
    options.bios.search_directory = path("/User/bios");
    // Let DuckStation choose from the installed BIOS files by region and compatibility.
    options.bios.path_ntscu = "";
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
    options.memorycards.card1_path = "polymer.mcd";
    options.memorycards.card2_type = "None";
    options.memorycards.directory = path("/User/memcards");
    options.memorycards.fast_forward_access = false;
    options.logging.log_level = "Info";
    options.logging.log_to_console = false;
    options.logging.log_to_file = true;
    options.logging.memory_card = true;
    options.logging.system = true;
    options.input_sources.sdl = true;
    options.gamelist.recursive_paths = path("/Game");
    options.autoupdater.check_at_startup = false;
    const settings = args.configuration.render()["settings.ini"];
    if (!settings) throw new TypeError("DuckStation configuration did not render settings.ini");
    args.filesystem.mount("/DuckStation/settings.ini", settings, { mode: "rw" });
    return {
        command: path("/DuckStation/duckstation-qt-x64-ReleaseLTCG.exe"),
        workingDirectory: args.filesystem.path("/DuckStation"),
        env: [
            ["USERPROFILE", args.filesystem.path("/User")],
            ["APPDATA", args.filesystem.path("/User/AppData/Roaming")],
            ["LOCALAPPDATA", args.filesystem.path("/User/AppData/Local")],
            ["TEMP", args.filesystem.path("/User/Temp")],
            ["TMP", args.filesystem.path("/User/Temp")],
        ],
        argv: ["-fastboot", "--", discPaths[args.initialDiscIndex]],
        isolate: true,
        hooks: { graphics: ["d3d11"], input: ["sdl3"] },
        surfaces: [{ index: 0 }],
    };
};

export const onParseHotkey = (expression: string) => HotkeyParser.parse(expression);

export const onPauseResume = (context: PauseResumeContext) => {
    context.actions.pause?.activate();
};

export const onDiscChange = (context: DiscChangeContext) => {
    const steps = context.index - currentDisc;
    const action = steps > 0 ? context.actions.next_disc : context.actions.previous_disc;
    for (let press = 0; press < Math.abs(steps); press++) action.activate();
    currentDisc = context.index;
};

export default { onPluginLoad, onUpdateContents, onLaunch, onParseHotkey, onPauseResume, onDiscChange };
