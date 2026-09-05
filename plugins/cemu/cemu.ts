import graphicsPacks from "pkg:bin/graphicsPacks";
import { caches } from "polymer:cache";
import { pluginStorage } from "polymer:pluginStorage";
import type {
    Configuration as HostConfiguration,
    ConfigurationResult,
    ConfigureGame,
    Filesystem,
    Resource,
    RepeatedSection,
    UserConfiguration,
} from "polymer:api";
import {
    loadGraphicsPacks,
    normalizeTitleId,
    settleGraphicsPacks,
    type GraphicsApi,
    type GraphicsPack,
} from "pkg:graphics-packs.ts";

type Configuration = HostConfiguration<{
    content: {
        open_pad: boolean,
        graphics_packs: RepeatedSection<{
            filename: string,
            enabled: boolean,
            presets: RepeatedSection<{
                category: string,
                preset: string,
            }>,
        }>,
    },
    "content-graphic": { api: number },
}>;

type EmulatorLaunchArgs = {
    launchPath: string,
    console: string,
    package: Readonly<{ titleId?: string }>,
    configuration: Configuration,
    userConfiguration?: UserConfiguration,
    filesystem: Filesystem,
    storage: Readonly<{
        slc?: Resource,
        slccmpt?: Resource,
        mlc?: Resource,
        sd?: Resource,
    }>,
};

const GRAPHICS_GROUP = "graphics";
const GRAPHICS_PACKS_STORAGE_KEY = "graphicsPacks";

export async function onPluginLoad() {
    pluginStorage.setItem(
        GRAPHICS_PACKS_STORAGE_KEY,
        await loadGraphicsPacks(graphicsPacks),
    );
}

export function onConfigure(
    game: ConfigureGame,
    userConfiguration?: UserConfiguration,
): ConfigurationResult {
    const api = selectedApi(userConfiguration);
    const groups: Array<ConfigurationResult["groups"][number]> = [{
        key: GRAPHICS_GROUP,
        title: "Graphics Options",
        options: [{
            key: "api",
            title: "Graphics API",
            type: "select",
            default: "vulkan",
            choices: [
                { key: "vulkan", label: "Vulkan" },
                { key: "opengl", label: "OpenGL" },
            ],
        }],
    }];
    const values: Record<string, Record<string, boolean | number | string | null>> = {
        [GRAPHICS_GROUP]: { api },
    };
    if (normalizeTitleId(game.titleId)) {
        const packs = loadedGraphicsPacks();
        const settled = settleGraphicsPacks(packs, game.titleId, userConfiguration, api);
        groups.push(...settled.groups);
        Object.assign(values, settled.values);
    }
    return {
        schema: "cemu/v1",
        key: normalizeTitleId(game.titleId) || game.serial || game.launchPath,
        groups,
        values,
    };
}

export function onLaunch(args: EmulatorLaunchArgs) {
    const userConfiguration = validUserConfiguration(args.userConfiguration);
    const api = selectedApi(userConfiguration);
    args.configuration.options().content.open_pad = true;
    args.configuration.options()["content-graphic"].api = api === "vulkan" ? 1 : 0;

    const packs = normalizeTitleId(args.package.titleId) ? loadedGraphicsPacks() : [];
    const selected = settleGraphicsPacks(
        packs,
        args.package.titleId,
        userConfiguration,
        api,
    ).entries;
    for (const selectedPack of selected) {
        const entry = args.configuration.options().content.graphics_packs.append();
        entry.filename = selectedPack.filename;
        entry.enabled = true;
        for (const selectedPreset of selectedPack.presets) {
            const preset = entry.presets.append();
            if (selectedPreset.category) preset.category = selectedPreset.category;
            preset.preset = selectedPreset.preset;
        }
    }

    args.filesystem.mountScratch("/Cemu", { mode: "rwx", backing: "temporary" });
    args.filesystem.mountFile("/Cemu/Cemu.exe", new URL("pkg:bin/win-x64/Cemu.exe"), { mode: "rx" });
    args.filesystem.mountTree("/Cemu/resources", new URL("pkg:bin/win-x64/resources"), { mode: "rx" });
    args.filesystem.mountTree("/Cemu/gameProfiles", new URL("pkg:bin/win-x64/gameProfiles"), { mode: "rx" });
    args.filesystem.mountTree("/Cemu/graphicPacks/downloadedGraphicPacks", graphicsPacks, { mode: "rx" });
    args.filesystem.mountTree("/Cemu/shaderCache", caches.shaderCache, { mode: "rwx" });
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
        hooks: { graphics: [api], input: ["sdl3"] },
        surfaces: [
            { index: 0, pattern: "Cemu+" },
            { index: 1, pattern: "GamePad+" },
        ],
    };
}

function selectedApi(configuration: UserConfiguration | undefined): GraphicsApi {
    return configuration?.[GRAPHICS_GROUP]?.api === "opengl" ? "opengl" : "vulkan";
}

function loadedGraphicsPacks(): GraphicsPack[] {
    const packs = pluginStorage.getItem<GraphicsPack[]>(GRAPHICS_PACKS_STORAGE_KEY);
    if (packs === null) throw new Error("graphics packs were not loaded");
    return packs;
}

function validUserConfiguration(value: unknown): UserConfiguration {
    if (!value || typeof value !== "object" || Array.isArray(value)) return {};
    const result: Record<string, Record<string, boolean | number | string | null>> = {};
    for (const [group, options] of Object.entries(value)) {
        if (!options || typeof options !== "object" || Array.isArray(options)) continue;
        result[group] = Object.fromEntries(Object.entries(options).filter(([, selected]) =>
            selected === null || ["boolean", "number", "string"].includes(typeof selected)
        )) as Record<string, boolean | number | string | null>;
    }
    return result;
}

export default { onPluginLoad, onConfigure, onLaunch };
