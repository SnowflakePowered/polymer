import runtime from "pkg:bin/win-x64";
import type { Configuration, Filesystem, Package, Storage } from "polymer:api";

type LaunchArgs = {
    package: Package,
    filesystem: Filesystem,
    storage: Readonly<{ ctrnand?: Storage }>,
    configuration: Configuration<{ storage: { nand_directory: string, sdmc_directory: string } }>,
};

export const onLaunch = (args: LaunchArgs) => {
    if (!args.storage.ctrnand) {
        throw new Error("Select a Nintendo 3DS NAND and save repository before launching Azahar.");
    }
    const fs = args.filesystem;
    fs.mount("/Azahar", runtime, { mode: "rx" });
    fs.mount("/Azahar/user");
    // Polymer's 3DS volume contains both nand/ and sdmc/, including title tickets.
    fs.mount("/Storage", args.storage.ctrnand, { mode: "rwx" });
    fs.mount("/User");
    const game = fs.mount("/Game", args.package, { mode: "rx" });
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
        isolate: true,
        hooks: { graphics: ["vulkan"], input: ["sdl2"] },
        surfaces: [{ index: 0, pattern: "Primary Window" }, { index: 1, pattern: "Secondary Window" }],
    };
};

export default { onLaunch };
