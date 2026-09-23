import runtime from "pkg:bin/win-x64";
import { caches } from "polymer:cache";
import { Rpcs3Configuration } from "pkg:configuration.ts";
import type { Configuration, Filesystem, Package, Storage } from "polymer:api";

type LaunchArgs = {
    package: Package,
    filesystem: Filesystem,
    configuration: Configuration,
    storage: Readonly<{ hdd?: Storage, flash?: Storage }>,
};

export const onLaunch = (args: LaunchArgs) => {
    if (!args.storage.hdd) {
        throw new Error("Select a PlayStation 3 NAND and save repository for the HDD, and install system software before launching RPCS3.");
    }
    const fs = args.filesystem;
    fs.mount("/RPCS3", runtime, { mode: "rx" });
    fs.mount("/Storage/hdd", args.storage.hdd, { mode: "rwx" });
    if (args.storage.flash) fs.mount("/Storage/flash", args.storage.flash, { mode: "rwx" });
    // The PS3 NAND contains both HDD and flash trees; a separate flash selection
    // is optional and overrides only the firmware partitions.
    const flash = args.storage.flash ? "/Storage/flash" : "/Storage/hdd";
    fs.mount("/RPCS3/cache", caches.cache, { mode: "rwx" });
    for (const dir of ["config", "GuiConfigs", "captures", "savestates", "dev_hdd1", "log"]) fs.mount(`/RPCS3/${dir}`);
    for (const file of ["RPCS3.log", "RPCS3.log.gz", "TTY.log"]) fs.mount(`/RPCS3/${file}`, new Uint8Array(), { mode: "rw" });
    fs.mount("/User");
    let game = fs.mount("/Game", args.package, { mode: "rx" });
    if (game.toLowerCase().endsWith(".pkg")) {
        const title = [args.package.titleId, args.package.serial].find(id => id && /^[A-Z]{4}[0-9]{5}$/.test(id));
        if (!title) throw new Error("The PS3 package has no bootable title ID. Install it and launch its EBOOT.BIN.");
        game = fs.path(`/Storage/hdd/dev_hdd0/game/${title}/USRDIR/EBOOT.BIN`);
    }
    const vfs = {
        "/dev_hdd0/": `${fs.path("/Storage/hdd/dev_hdd0")}/`,
        "/dev_hdd1/": `${fs.path("/RPCS3/dev_hdd1")}/`,
        "/dev_flash/": `${fs.path(`${flash}/dev_flash`)}/`,
        "/dev_flash2/": `${fs.path(`${flash}/dev_flash2`)}/`,
        "/dev_flash3/": `${fs.path(`${flash}/dev_flash3`)}/`,
    };
    fs.mount("/RPCS3/vfs.yml", new TextEncoder().encode(JSON.stringify(vfs)), { mode: "rw" });
    Rpcs3Configuration.mount(args.configuration, fs);
    return {
        command: fs.path("/RPCS3/rpcs3.exe"),
        workingDirectory: fs.path("/RPCS3"),
        argv: ["--no-gui", "--user-id", "00000001", "--config", fs.path("/RPCS3/config.yml"), game],
        env: [["USERPROFILE", fs.path("/User")], ["APPDATA", fs.path("/User/AppData/Roaming")],
            ["LOCALAPPDATA", fs.path("/User/AppData/Local")], ["TEMP", fs.path("/User/Temp")],
            ["TMP", fs.path("/User/Temp")]],
        isolate: true,
        hooks: { graphics: ["vulkan"], input: ["sdl3"] },
        surfaces: [{ index: 0, pattern: "RPCS3|FPS.*Vulkan" }],
    };
};

export default { onLaunch };
