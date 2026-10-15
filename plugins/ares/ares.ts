import { caches, config } from "polymer:plugin";
import { connect } from "polymer:database";
import { download } from "polymer:download";
import { Job } from "polymer:jobs";
import { osIdentifier } from "polymer:sys";
import { unarchive } from "polymer:unarchive";
import { EmulatorUpdates } from "jsr:@polymer-fe/emulator-updates@^0.1.0";
import { systems } from "pkg:systems.ts";
import type { BatteryStorage, ConfigurationResult, EmulatorLaunchArgs, Package, Storage, UserConfiguration } from "polymer:api";

type LaunchArgs = EmulatorLaunchArgs<{
    video: { driver: string, quality: string },
    input: { defocus: string, driver: string },
    general: { auto_save_memory: boolean, no_file_prompt: boolean, show_status_bar: boolean },
    paths: { home: string, saves: string, firmware: string, screenshots: string },
}, BatteryStorage & Readonly<{ card1?: Storage }>>;

const runtime = new EmulatorUpdates({
    name: "ares",
    cache: caches.runtime,
    osIdentifier,
    channel: String(config.channel ?? "stable"),
    connect,
    download,
    unarchive,
    Job,
});

export const onPluginLoad = () => runtime.ready();

export const onUpdateContents = async () => runtime.update([await runtime.release("ares")]);

const N64_PLATFORMS = ["NINTENDO_N64", "NINTENDO_N64DD"];
const N64_RESOLUTIONS = [
    { key: "SD", label: "1x Native" },
    { key: "HD", label: "2x Native" },
    { key: "UHD", label: "4x Native" },
];

export const onConfigure = (pkg: Package, userConfiguration?: UserConfiguration): ConfigurationResult => ({
    schema: "ares/v1",
    groups: N64_PLATFORMS.includes(pkg.platform) ? [{
        key: "graphics",
        title: "Graphics",
        options: [{
            key: "resolution",
            title: "Internal Resolution",
            type: "select",
            default: "SD",
            choices: N64_RESOLUTIONS,
        }],
    }] : [],
    values: N64_PLATFORMS.includes(pkg.platform) ? { graphics: { resolution: n64Resolution(userConfiguration) } } : {},
});

function n64Resolution(configuration?: UserConfiguration): string {
    const value = configuration?.graphics?.resolution;
    return N64_RESOLUTIONS.some(choice => choice.key === value) ? value as string : "SD";
}

export const onLaunch = async (args: LaunchArgs) => {
    const system = systems[args.console];
    if (!system) throw new Error(`ares does not support ${args.console}`);
    const fs = args.filesystem;
    fs.mount("/ares", await runtime.launch(), { mode: "rx" });
    fs.mount("/User");
    fs.mount("/User/saves", caches.saves, { mode: "rw" });
    const game = fs.mount("/Game", args.launchSet[0].package, { mode: "rx" });
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
    if (N64_PLATFORMS.includes(args.console)) options.video.quality = n64Resolution(args.userConfiguration);
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
        env: [["USERPROFILE", fs.path("/User")], ["APPDATA", fs.path("/User/AppData/Roaming")],
            ["LOCALAPPDATA", fs.path("/User/AppData/Local")], ["TEMP", fs.path("/User/Temp")],
            ["TMP", fs.path("/User/Temp")]],
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

export default { onPluginLoad, onUpdateContents, onConfigure, onLaunch };
