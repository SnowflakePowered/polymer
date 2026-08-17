// Host-backed view of snowglass-config's ConfigurationInstance. This object is
// deliberately opaque: the plugin may inspect and adjust option values, but it
// returns the same object to pluginhost, which alone drives serialization.
type Configuration = {
    options(): {
        content: {
            open_pad: boolean,
        },
    },
    render(): Record<string, Uint8Array>,
}

type Resource = Readonly<{ kind: "tree" | "file", path?: string }>;

type Filesystem = {
    mountHostTree(target: string, source: string, options: { mode: string }): void,
    mountHostFile(target: string, source: string, options: { mode: string }): void,
    mountTree(target: string, source: Resource | URL | string, options: { mode: string }): void,
    mountFile(target: string, source: Resource | URL | string | Uint8Array, options: { mode: string }): void,
    mountScratch(target: string, options: { mode: string, backing: "memory" | "temporary" }): void,
    path(path: string): string,
    setWorkingDirectory(path: string): void,
    setProfile(profile: {
        home?: string,
        roamingAppData?: string,
        localAppData?: string,
        temp?: string,
    }): void,
};

// We'll need to determine the shape of the NAND API later as well.
type SetupLaunchArgs = {
    launchPath: string,
    console: string,
    portAssignments: { [group: string]: string[] },
    configuration: Configuration,
    filesystem: Filesystem,
    resources: { runtime: Resource },
}

export const setupLaunch = (args: SetupLaunchArgs) => {
    args.configuration.options().content.open_pad = true;

    // The package files are recursive read-only submounts over a writable,
    // disposable Cemu directory. This lets Cemu create arbitrary caches and
    // diagnostics beside its executable without mutating the package.
    args.filesystem.mountScratch("/Cemu", { mode: "rwx", backing: "temporary" });
    args.filesystem.mountFile("/Cemu/Cemu.exe", new URL("pkg:bin/win-x64/Cemu.exe"), { mode: "rx" });
    args.filesystem.mountTree("/Cemu/resources", new URL("pkg:bin/win-x64/resources"), { mode: "rx" });
    args.filesystem.mountTree("/Cemu/gameProfiles", new URL("pkg:bin/win-x64/gameProfiles"), { mode: "rx" });
    for (const [name, bytes] of Object.entries(args.configuration.render())) {
        args.filesystem.mountFile(`/Cemu/${name}`, bytes, { mode: "rw" });
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
        configRoot: args.filesystem.path("/Cemu"),
        isolate: true,
        hooks: ["vulkan", "opengl"],
        // pluginhost unwraps and serializes this after setupLaunch returns.
        config: args.configuration,
        surfaces: [
            { index: 0, pattern: "Cemu+" },
            { index: 1, pattern: "GamePad+" }
        ]
    }
}

export default {
    setupLaunch
}
