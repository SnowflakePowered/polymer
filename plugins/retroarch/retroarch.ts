import type { EmulatorLaunchArgs } from "polymer:api";
import { caches, config } from "polymer:plugin";
import { connect } from "polymer:database";
import { download } from "polymer:download";
import { Job } from "polymer:jobs";
import { osIdentifier } from "polymer:sys";
import { unarchive } from "polymer:unarchive";
import { EmulatorUpdates } from "jsr:@polymer-fe/emulator-updates@^0.1.0";

const runtime = new EmulatorUpdates({
    name: "RetroArch",
    cache: caches.runtime,
    osIdentifier,
    channel: String(config.channel ?? "stable"),
    connect,
    download,
    unarchive,
    Job,
});

export const onPluginLoad = () => runtime.ready();

export const onUpdateContents = async () => runtime.update([await runtime.release("retroarch"), {
    version: "latest",
    url: "https://buildbot.libretro.com/nightly/windows/x86_64/latest/pcsx2_libretro.dll.zip",
    directory: "cores",
}]);

export const onLaunch = async (args: EmulatorLaunchArgs<{
    retroarch: {
        core_options_path: string,
        system_directory: string,
        savefile_directory: string,
        savestate_directory: string,
        log_dir: string,
        libretro_directory: string,
        libretro_info_path: string,
        assets_directory: string,
    },
    core: { pcsx2_bios: string },
}>) => {
    const bios = (await args.firmwareStore.list()).find(entry =>
        entry.platform === "SONY_PS2" && entry.byteLength === 4 * 1024 * 1024
    );
    if (!bios) {
        throw new Error("PCSX2 requires a PS2 BIOS. Drag and drop a PS2 BIOS file into Polymer, then launch again.");
    }
    const biosFile = await args.firmwareStore.open(bios.label);
    const biosName = "bios.bin";

    args.filesystem.mount("/RetroArch", await runtime.launch(), { mode: "rx" });
    args.filesystem.mount("/User/saves");
    args.filesystem.mount("/User/states");
    args.filesystem.mount("/User/logs");
    args.filesystem.mount("/User/system/pcsx2/memcards");
    args.filesystem.mount(`/User/system/pcsx2/bios/${biosName}`, biosFile, { mode: "r" });
    const gamePath = args.filesystem.mount("/Game", args.launchSet[0].package, { mode: "rx" });

    const options = args.configuration.options();
    options.retroarch.core_options_path = args.filesystem.path("/User/core-options.cfg");
    options.retroarch.system_directory = args.filesystem.path("/User/system");
    options.retroarch.savefile_directory = args.filesystem.path("/User/saves");
    options.retroarch.savestate_directory = args.filesystem.path("/User/states");
    options.retroarch.log_dir = args.filesystem.path("/User/logs");
    options.retroarch.libretro_directory = args.filesystem.path("/RetroArch/cores");
    options.retroarch.libretro_info_path = args.filesystem.path("/RetroArch/info");
    options.retroarch.assets_directory = args.filesystem.path("/RetroArch/assets");
    options.core.pcsx2_bios = biosName;
    for (const [name, bytes] of Object.entries(args.configuration.render({ ini: { quote: "all", spaces: true } }))) {
        args.filesystem.mount(`/User/${name}`, bytes, { mode: "rw" });
    }

    return {
        command: args.filesystem.path("/RetroArch/retroarch.exe"),
        workingDirectory: args.filesystem.path("/RetroArch"),
        env: [
            ["USERPROFILE", args.filesystem.path("/User")],
            ["APPDATA", args.filesystem.path("/User/AppData/Roaming")],
            ["LOCALAPPDATA", args.filesystem.path("/User/AppData/Local")],
            ["TEMP", args.filesystem.path("/User/Temp")],
            ["TMP", args.filesystem.path("/User/Temp")],
        ],
        argv: [
            "-c", args.filesystem.path("/User/retroarch.cfg"),
            "-L", args.filesystem.path("/RetroArch/cores/pcsx2_libretro.dll"),
            "--verbose",
            "--log-file", args.filesystem.path("/User/logs/lrps2.log"),
            "-f",
            gamePath,
        ],
        isolate: true,
        hooks: { graphics: ["d3d11"], input: ["sdl3"] },
        surfaces: [
            { index: 0, pattern: "RetroArch" },
        ],
    };
};

export default { onPluginLoad, onUpdateContents, onLaunch };
