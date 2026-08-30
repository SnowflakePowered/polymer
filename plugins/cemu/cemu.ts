import type { Configuration as HostConfiguration, Filesystem, Resource } from "../plugin-api.ts";

// Host-backed view of snowglass-config's ConfigurationInstance. This object is
// deliberately opaque: the plugin may inspect and adjust option values, but it
// returns the same object to pluginhost, which alone drives serialization.
type Configuration = HostConfiguration<{
        content: {
            open_pad: boolean,
        },
}>;

type EmulatorLaunchArgs = {
    launchPath: string,
    console: string,
    configuration: Configuration,
    filesystem: Filesystem,
    resources: { runtime: Resource },
    storage: Readonly<{
        slc?: Resource,
        slccmpt?: Resource,
        mlc?: Resource,
        sd?: Resource,
    }>,
}

export const onEmulatorLaunch = (args: EmulatorLaunchArgs) => {
    args.configuration.options().content.open_pad = true;

    // The package files are recursive read-only submounts over a writable,
    // disposable Cemu directory. This lets Cemu create arbitrary caches and
    // diagnostics beside its executable without mutating the package.
    args.filesystem.mountScratch("/Cemu", { mode: "rwx", backing: "temporary" });
    args.filesystem.mountFile("/Cemu/Cemu.exe", new URL("pkg:bin/win-x64/Cemu.exe"), { mode: "rx" });
    args.filesystem.mountTree("/Cemu/resources", new URL("pkg:bin/win-x64/resources"), { mode: "rx" });
    args.filesystem.mountTree("/Cemu/gameProfiles", new URL("pkg:bin/win-x64/gameProfiles"), { mode: "rx" });
    args.filesystem.mountTree("/Cemu/shaderCache", args.filesystem.mountCache("shaderCache"), { mode: "rwx" });
    for (const [name, bytes] of Object.entries(args.configuration.render())) {
        args.filesystem.mountFile(`/Cemu/${name}`, bytes, { mode: "rw" });
    }
    if (args.storage.mlc) {
        args.filesystem.mountStorage("/Cemu/mlc01", args.storage.mlc, { mode: "rwx" });
    }
    args.filesystem.mountHostTree("/Game", args.launchPath, { mode: "rx" });
    args.filesystem.mountScratch("/User", { mode: "rwx", backing: "temporary" });
    args.filesystem.setWorkingDirectory("/Cemu");
    args.filesystem.setProfile({
        home: "/User",
        roamingAppData: "/User/AppData/Roaming",
        localAppData: "/User/AppData/Local",
        temp: "/User/Temp",
    });

    const executable = args.filesystem.path("/Cemu/Cemu.exe");
    const game = args.filesystem.path("/Game");
    const mlc = args.filesystem.path("/Cemu/mlc01");

    return {
        command: executable,
        argv: ["-g", game, "--mlc", mlc],
        isolate: true,
        hooks: ["vulkan", "opengl"],
        surfaces: [
            { index: 0, pattern: "Cemu+" },
            { index: 1, pattern: "GamePad+" }
        ]
    }
}

export default {
    onEmulatorLaunch
}
