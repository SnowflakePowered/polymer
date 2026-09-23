import runtime from "pkg:bin/win-x64";
import { caches } from "polymer:cache";
import type { Configuration, Filesystem, FirmwareContext, Package } from "polymer:api";

type LaunchArgs = FirmwareContext & {
    package: Package,
    filesystem: Filesystem,
    configuration: Configuration<{
        system: { InternalBackupRAMImagePath: string },
        ipl: { Path: string },
    }>,
};

export const onLaunch = async (args: LaunchArgs) => {
    const bios = (await args.firmwareStore.list()).find(file => file.byteLength === 512 * 1024);
    if (!bios) throw new Error("Ymir requires a Saturn BIOS. Import a Saturn BIOS into Polymer first.");
    const fs = args.filesystem;
    fs.mount("/Ymir", runtime, { mode: "rx" });
    fs.mount("/Profile");
    fs.mount("/Profile/state", caches.saves, { mode: "rw" });
    fs.mount("/Profile/bios.bin", await args.firmwareStore.open(bios.label), { mode: "r" });
    fs.mount("/User");
    const game = fs.mount("/Game", args.package, { mode: "rx" });
    const options = args.configuration.options();
    options.system.InternalBackupRAMImagePath = JSON.stringify(fs.path("/Profile/state/bup-int.bin"));
    options.ipl.Path = JSON.stringify(fs.path("/Profile/bios.bin"));
    // The host's INI emitter can carry TOML scalar/array literals. Expand the
    // native table paths here because host target paths identify separate nodes.
    const rendered = new TextDecoder().decode(args.configuration.render()["Ymir.toml"])
        .replace("[IPL]", "[System.IPL]")
        .replace(/\[Type([12])\]/g, "[Input.Port$1]")
        .replace(/\[Port([12])\]/g, "[Input.Port$1.ControlPad.Binds]");
    const config = `${rendered}\n[General]\nPauseWhenUnfocused = false\nCheckForUpdates = false\n`;
    fs.mount("/Profile/Ymir.toml", new TextEncoder().encode(config), { mode: "rw" });
    return {
        command: fs.path("/Ymir/ymir-sdl3.exe"),
        workingDirectory: fs.path("/Ymir"),
        argv: ["--profile", fs.path("/Profile"), "--disc", game],
        env: [["SDL_RENDER_DRIVER", "direct3d11"], ["USERPROFILE", fs.path("/User")],
            ["APPDATA", fs.path("/User/AppData/Roaming")], ["LOCALAPPDATA", fs.path("/User/AppData/Local")],
            ["TEMP", fs.path("/User/Temp")], ["TMP", fs.path("/User/Temp")]],
        isolate: true,
        hooks: { graphics: ["d3d11"], input: ["sdl3"] },
        surfaces: [{ index: 0, pattern: "Ymir" }],
    };
};

export default { onLaunch };
