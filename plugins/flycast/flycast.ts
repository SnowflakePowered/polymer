import runtime from "pkg:bin/win-x64";
import discScript from "pkg:disc.lua";
import type { DiscChangeContext, EmulatorLaunchArgs, Storage } from "polymer:api";

const DISC_REQUEST = "/Flycast/polymer-disc.txt";
let discPaths: string[] = [];

export const onLaunch = async (args: EmulatorLaunchArgs<unknown, Readonly<{ vmu?: Storage }>>) => {
    const path = (virtualPath: string) => args.filesystem.path(virtualPath);

    args.filesystem.mount("/Flycast", runtime, { mode: "rx" });
    args.filesystem.mount("/Flycast/data");
    for (const name of ["dc_boot.bin", "dc_flash.bin"]) {
        const file = await args.firmwareStore.open(name);
        args.filesystem.mount(`/Flycast/data/${name}`, file, { mode: "r" });
    }
    if (args.storage.vmu) {
        args.filesystem.mount("/Flycast/data/vmu_save_A1.bin", args.storage.vmu, {
            mode: "rw",
            format: "raw",
        });
    }
    discPaths = args.launchSet.length > 1
        ? args.launchSet.map((disc, index) => args.filesystem.mount(`/Game/Disc${index}`, disc.package, { mode: "rx" }))
        : [args.filesystem.mount("/Game", args.launchSet[0].package, { mode: "rx" })];
    const gamePath = discPaths[args.initialDiscIndex];
    if (discPaths.length > 1) {
        args.filesystem.mount("/Flycast/flycast.lua", discScript, { mode: "r" });
        args.filesystem.mount(DISC_REQUEST, new TextEncoder().encode(gamePath), { mode: "r" });
    }
    args.filesystem.mount("/User");
    for (const [name, bytes] of Object.entries(args.configuration.render())) {
        args.filesystem.mount(`/Flycast/${name}`, bytes, { mode: "rw" });
    }

    return {
        command: path("/Flycast/flycast.exe"),
        workingDirectory: args.filesystem.path("/Flycast"),
        env: [
            ["USERPROFILE", args.filesystem.path("/User")],
            ["APPDATA", args.filesystem.path("/User/AppData/Roaming")],
            ["LOCALAPPDATA", args.filesystem.path("/User/AppData/Local")],
            ["TEMP", args.filesystem.path("/User/Temp")],
            ["TMP", args.filesystem.path("/User/Temp")],
            ["POLYMER_DISC_REQUEST", path(DISC_REQUEST)],
        ],
        argv: [gamePath],
        isolate: false,
        hooks: { graphics: ["vulkan"], input: ["sdl2"] },
        surfaces: [{ index: 0, pattern: "Flycast" }],
    };
};

export const onDiscChange = (context: DiscChangeContext) => {
    context.filesystem.mount(DISC_REQUEST, new TextEncoder().encode(discPaths[context.index]), { mode: "r" });
    context.filesystem.flush();
};

export default { onLaunch, onDiscChange };
