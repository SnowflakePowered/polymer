import runtime from "pkg:bin/win-x64";
import type { EmulatorLaunchArgs, Storage } from "polymer:api";

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
    const gamePath = args.filesystem.mount("/Game", args.package, { mode: "rx" });
    args.filesystem.mount("/User");
    for (const [name, bytes] of Object.entries(args.configuration.render())) {
        args.filesystem.mount(`/Flycast/${name}`, bytes, { mode: "rw" });
    }

    return {
        command: path("/Flycast/flycast.exe"),
        workingDirectory: args.filesystem.path("/Flycast"),
        argv: [gamePath],
        isolate: false,
        hooks: { graphics: ["vulkan"], input: ["sdl3"] },
        surfaces: [{ index: 0, pattern: "Flycast" }],
    };
};

export default { onLaunch };
