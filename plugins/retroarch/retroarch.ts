import runtime from "pkg:bin/win-x64";
import type { EmulatorLaunchArgs } from "polymer:api";

function quoted(value: string): string {
    return `"${value.replaceAll("\\", "\\\\").replaceAll("\"", "\\\"")}"`;
}

export const onLaunch = async (args: EmulatorLaunchArgs<{ core: { placeholder: boolean } }>) => {
    const bios = (await args.firmwareStore.list()).find(entry =>
        entry.platform === "SONY_PS2" && entry.byteLength === 4 * 1024 * 1024
    );
    if (!bios) {
        throw new Error("PCSX2 requires a PS2 BIOS. Drag and drop a PS2 BIOS file into Polymer, then launch again.");
    }
    const biosFile = await args.firmwareStore.open(bios.label);
    const biosName = "bios.bin";

    args.filesystem.mount("/RetroArch", runtime, { mode: "rx" });
    args.filesystem.mount("/User/saves");
    args.filesystem.mount("/User/states");
    args.filesystem.mount("/User/logs");
    args.filesystem.mount("/User/system/pcsx2/memcards");
    args.filesystem.mount(`/User/system/pcsx2/bios/${biosName}`, biosFile, { mode: "r" });
    const gamePath = args.filesystem.mount("/Game", args.package, { mode: "rx" });

    const runtimeConfig = [
        `config_save_on_exit = "false"`,
        `core_info_cache_enable = "false"`,
        `core_options_path = ${quoted(args.filesystem.path("/User/core-options.cfg"))}`,
        `system_directory = ${quoted(args.filesystem.path("/User/system"))}`,
        `savefile_directory = ${quoted(args.filesystem.path("/User/saves"))}`,
        `savestate_directory = ${quoted(args.filesystem.path("/User/states"))}`,
        `log_dir = ${quoted(args.filesystem.path("/User/logs"))}`,
        `libretro_directory = ${quoted(args.filesystem.path("/RetroArch/cores"))}`,
        `libretro_info_path = ${quoted(args.filesystem.path("/RetroArch/info"))}`,
        `assets_directory = ${quoted(args.filesystem.path("/RetroArch/assets"))}`,
        `video_driver = "d3d11"`,
        `video_fullscreen = "true"`,
        `input_autodetect_enable = "true"`,
        `pause_nonactive = "false"`,
        `quit_on_close_content = "1"`,
        "",
    ].join("\n");
    args.filesystem.mount(
        "/User/retroarch.cfg",
        new TextEncoder().encode(runtimeConfig),
        { mode: "r" },
    );
    args.filesystem.mount(
        "/User/core-options.cfg",
        new TextEncoder().encode([
            `pcsx2_bios = ${quoted(biosName)}`,
            `pcsx2_renderer = "D3D11"`,
            `pcsx2_fastboot = "enabled"`,
            `pcsx2_shared_memory_cards = "enabled"`,
            "",
        ].join("\n")),
        { mode: "r" },
    );

    const options = args.configuration.options();
    options.core.placeholder = false;
    for (const [name, bytes] of Object.entries(args.configuration.render())) {
        args.filesystem.mount(`/Generated/${name}`, bytes, { mode: "rw" });
    }

    return {
        command: args.filesystem.path("/RetroArch/retroarch.exe"),
        workingDirectory: args.filesystem.path("/RetroArch"),
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

export default { onLaunch };
