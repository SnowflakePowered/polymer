import runtime from "pkg:bin/win-x64";
import { caches } from "polymer:cache";
import { systems } from "pkg:systems.ts";
import type { BatteryStorage, EmulatorLaunchArgs, Storage } from "polymer:api";

type LaunchArgs = EmulatorLaunchArgs<{
    video: { driver: string },
    input: { defocus: string, driver: string },
    general: { auto_save_memory: boolean, no_file_prompt: boolean, show_status_bar: boolean },
    paths: { home: string, saves: string, firmware: string, screenshots: string },
}, BatteryStorage & Readonly<{ card1?: Storage }>>;

export const onLaunch = async (args: LaunchArgs) => {
    const system = systems[args.console];
    if (!system) throw new Error(`ares does not support ${args.console}`);
    const fs = args.filesystem;
    fs.mount("/ares", runtime, { mode: "rx" });
    fs.mount("/User");
    fs.mount("/User/saves", caches.saves, { mode: "rw" });
    const game = fs.mount("/Game", args.package, { mode: "rx" });
    const filename = game.replaceAll("\\", "/").split("/").at(-1)!;
    const stem = filename.replace(/\.[^.]+$/, "");
    if (args.storage.battery && system.battery) {
        fs.mount(`/User/saves/${system.battery[0]}/${stem}${system.battery[1]}`, args.storage.battery, { mode: "rw" });
    }
    if (args.console === "SONY_PSX" && args.storage.card1) {
        fs.mount(`/User/saves/PlayStation/${stem}.card`, args.storage.card1, { mode: "rw", format: "raw" });
    }

    const options = args.configuration.options();
    options.video.driver = "OpenGL 3.2";
    options.input.defocus = "Allow";
    options.input.driver = "SDL";
    options.general.auto_save_memory = true;
    options.general.no_file_prompt = true;
    options.general.show_status_bar = false;
    // ares concatenates filenames onto these paths, so retain the trailing slash.
    options.paths.home = `${fs.path("/User/saves/Systems")}/`;
    options.paths.saves = `${fs.path("/User/saves")}/`;
    options.paths.firmware = `${fs.path("/User/firmware")}/`;
    options.paths.screenshots = `${fs.path("/User/screenshots")}/`;
    const firmware = await AresFirmware.mount(args);
    const settings = new TextDecoder().decode(args.configuration.render()["settings.bml"]);
    fs.mount("/User/settings.bml", new TextEncoder().encode(settings + firmware), { mode: "rw" });
    return {
        command: fs.path("/ares/ares.exe"),
        workingDirectory: fs.path("/ares"),
        argv: ["--kiosk", "--settings-file", fs.path("/User/settings.bml"), "--system", system.name, game],
        isolate: true,
        hooks: { graphics: ["opengl"], input: ["sdl3"] },
        surfaces: [{ index: 0, pattern: "ares" }],
    };
};

class AresFirmware {
    static async mount(args: LaunchArgs): Promise<string> {
        const system = systems[args.console];
        if (system.firmware.length === 0) return "";
        const files = await args.firmwareStore.list();
        const available = await Promise.all(files.map(async file => {
            const resource = await args.firmwareStore.open(file.label);
            return { resource, sha256: await resource.digest("sha2") };
        }));
        const lines = [`\n${(system.firmwareSystem ?? system.name).replaceAll(/[ ()]/g, "")}\n  Firmware`];
        for (const [slot, hash] of system.firmware) {
            const file = hash ? available.find(file => file.sha256 === hash) : available[0];
            if (!file) continue;
            const name = `/User/firmware/${slot}.bin`;
            args.filesystem.mount(name, file.resource, { mode: "r" });
            lines.push(`    ${slot}: ${args.filesystem.path(name)}`);
        }
        if (lines.length === 1 && ["COLECO_CV", "NEC_TGCD", "NINTENDO_FDS", "NINTENDO_GBA", "NINTENDO_N64DD", "SEGA_32X_CD", "SEGA_CD", "SEGA_SAT", "SNK_NG", "SNK_NGP", "SNK_NGPC", "SONY_PSX"].includes(args.console)) {
            throw new Error(`ares requires a compatible ${system.name} BIOS. Import the firmware into Polymer first.`);
        }
        return `${lines.join("\n")}\n`;
    }
}

export default { onLaunch };
