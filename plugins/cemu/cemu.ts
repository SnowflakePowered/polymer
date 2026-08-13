import config from "pkg:plugin.toml";

// Host-backed view of snowglass-config's ConfigurationInstance. This object is
// deliberately opaque: the plugin may inspect and adjust option values, but it
// returns the same object to pluginhost, which alone drives serialization.
type Configuration = {
    options(): {
        content: {
            open_pad: boolean,
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
    const executable = new URL(config.plugin.config.cemu_path);
    args.emulatorConfig.options().content.open_pad = true;

    return {
        command: executable.href,
        argv: ["-g", args.launchPath],
        configRoot: new URL(".", executable).href,
        isolate: true,
        hooks: ["vulkan", "opengl"],
        // pluginhost unwraps and serializes this after setupLaunch returns.
        config: args.emulatorConfig,
        surfaces: [
            { index: 0, pattern: "Cemu+" },
            { index: 1, pattern: "GamePad+" }
        ]
    }
}

export default {
    setupLaunch
}
