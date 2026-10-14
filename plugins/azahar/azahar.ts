import { caches, config } from "polymer:plugin";
import { connect } from "polymer:database";
import { download } from "polymer:download";
import { Job } from "polymer:jobs";
import { osIdentifier } from "polymer:sys";
import { unarchive } from "polymer:unarchive";
import { EmulatorUpdates } from "jsr:@polymer-fe/emulator-updates@^0.1.0";
import type { EmulatorLaunchArgs, PauseResumeContext, Storage } from "polymer:api";
import { HotkeyParser } from "pkg:hotkeys.ts";

const runtime = new EmulatorUpdates({
    name: "Azahar",
    cache: caches.runtime,
    osIdentifier,
    channel: String(config.channel ?? "stable"),
    connect,
    download,
    unarchive,
    Job,
});

export const onPluginLoad = () => runtime.ready();

export const onUpdateContents = async () => runtime.update([await runtime.release("azahar")]);

export const onLaunch = async (args: EmulatorLaunchArgs<
    { storage: { nand_directory: string, sdmc_directory: string } },
    Readonly<{ ctrnand?: Storage }>
>) => {
    if (!args.storage.ctrnand) {
        throw new Error("Select a Nintendo 3DS NAND and save repository before launching Azahar.");
    }
    const fs = args.filesystem;
    fs.mount("/Azahar", await runtime.launch(), { mode: "rx" });
    fs.mount("/Azahar/user");
    // Polymer's 3DS volume contains both nand/ and sdmc/, including title tickets.
    fs.mount("/Storage", args.storage.ctrnand, { mode: "rwx" });
    fs.mount("/User");
    const game = fs.mount("/Game", args.launchSet[0].package, { mode: "rx" });
    const options = args.configuration.options().storage;
    // QSettings treats backslashes as escapes and commas as list separators.
    options.nand_directory = JSON.stringify(fs.path("/Storage/nand").replaceAll("\\", "/"));
    options.sdmc_directory = JSON.stringify(fs.path("/Storage/sdmc").replaceAll("\\", "/"));
    for (const [name, bytes] of Object.entries(args.configuration.render())) {
        fs.mount(`/Azahar/user/config/${name}`, bytes, { mode: "rw" });
    }
    return {
        command: fs.path("/Azahar/azahar.exe"),
        workingDirectory: fs.path("/Azahar"),
        argv: [game],
        env: [["USERPROFILE", fs.path("/User")], ["APPDATA", fs.path("/User/AppData/Roaming")],
            ["LOCALAPPDATA", fs.path("/User/AppData/Local")], ["TEMP", fs.path("/User/Temp")],
            ["TMP", fs.path("/User/Temp")]],
        isolate: false,
        hooks: { graphics: ["vulkan"], input: ["sdl2"] },
        surfaces: [{ index: 0, pattern: "^Azahar [^|]*\\| [^|]*$" }, { index: 1, pattern: "\\| Secondary Window$" }],
    };
};

export const onParseHotkey = (expression: string) => HotkeyParser.parse(expression);

export const onPauseResume = (context: PauseResumeContext) => {
    context.actions.pause?.activate();
};

export default { onPluginLoad, onUpdateContents, onLaunch, onParseHotkey, onPauseResume };
