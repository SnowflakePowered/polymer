import runtime from "pkg:bin/win-x64";
import type { Package, Configuration as HostConfiguration, FirmwareContext, Filesystem, Storage } from "polymer:api";

type Configuration = HostConfiguration;

type EmulatorLaunchArgs = FirmwareContext & {
    launchPath: string,
    package: Package,
    console: string,
    configuration: Configuration,
    filesystem: Filesystem,
    storage: Readonly<{ vmu?: Storage }>,
};

export const onLaunch = async (args: EmulatorLaunchArgs) => {
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
        env: [
            ["USERPROFILE", args.filesystem.path("/User")],
            ["APPDATA", args.filesystem.path("/User/AppData/Roaming")],
            ["LOCALAPPDATA", args.filesystem.path("/User/AppData/Local")],
            ["TEMP", args.filesystem.path("/User/Temp")],
            ["TMP", args.filesystem.path("/User/Temp")],
        ],
        argv: [gamePath],
        isolate: false,
        hooks: { graphics: ["vulkan"], input: ["sdl3"] },
        surfaces: [{ index: 0, pattern: "Flycast" }],
    };
};

export default { onLaunch };
