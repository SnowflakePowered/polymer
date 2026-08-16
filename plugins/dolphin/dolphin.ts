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
            input: {
                background_input: boolean,
            },
        },
    },
}

// We'll need to determine the shape of the NAND API later as well.
type SetupLaunchArgs = {
    launchPath: string,
    console: string,
    portAssignments: { [group: string]: string[] },
    configuration: Configuration
}

export const setupLaunch = (args: SetupLaunchArgs) => {
    const executable = new URL(config.plugin.config.dolphin_path);
    args.configuration.options().main.core.gfx_backend = "D3D";
    // Snowglass owns the focused host window while Dolphin renders through an
    // isolated capture surface. Dolphin must continue polling the injected
    // virtual controller while its own hidden window is not foreground.
    args.configuration.options().main.input.background_input = true;

    return {
        command: executable.href,
        argv: ["-b", "-e", args.launchPath],
        // Dolphin's portable distribution keeps its writable INI hierarchy in
        // `User/`. Pointing at the executable directory writes a parallel
        // `Config/` tree that Dolphin never reads, leaving its old direct-input
        // mappings active instead of the Snowglass virtual controller.
        configRoot: new URL("User/", executable).href,
        isolate: true,
        hooks: ["d3d11"],
        config: args.configuration,
        surfaces: [
            { index: 0, pattern: "Dolphin" },
        ]
    }
}

export default {
    setupLaunch
}
