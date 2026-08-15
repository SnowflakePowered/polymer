import config from "pkg:plugin.toml";

// Host-backed view of snowglass-config's ConfigurationInstance. This object is
// deliberately opaque: the plugin may inspect and adjust option values, but it
// returns the same object to pluginhost, which alone drives serialization.
type Configuration = {
    options(): {
        main: {
            core: {
                gfx_backend: string,
            },
        },
    },
}

// We'll need to determine the shape of the NAND API later as well.
type SetupLaunchArgs = {
    launchPath: string,
    console: string,
    portAssignments: { [group: string]: string[] },
    emulatorConfig: Configuration
}

export const setupLaunch = (args: SetupLaunchArgs) => {
    const executable = new URL(config.plugin.config.dolphin_path);
    args.emulatorConfig.options().main.core.gfx_backend = "D3D";

    return {
        command: executable.href,
        argv: ["-b", "-e", args.launchPath],
        configRoot: new URL(".", executable).href,
        isolate: true,
        hooks: ["d3d11"],
        config: args.emulatorConfig,
        surfaces: [
            { index: 0, pattern: "Dolphin" },
        ]
    }
}

export default {
    setupLaunch
}
