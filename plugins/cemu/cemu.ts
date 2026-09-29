import cemuExecutable from "pkg:bin/win-x64/Cemu.exe";
import graphicsPacks from "pkg:bin/graphicsPacks";
import resources from "pkg:bin/win-x64/resources";
import gameProfiles from "pkg:bin/win-x64/gameProfiles";
import { caches } from "polymer:cache";
import { pluginStorage } from "polymer:pluginStorage";
import type {
    ConfigurationResult,
    EmulatorLaunchArgs,
    Package,
    Storage,
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

type Options = {
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
};

type Storages = Readonly<{
    slc?: Storage,
    slccmpt?: Storage,
    mlc?: Storage,
    sd?: Storage,
}>;

const GRAPHICS_GROUP = "graphics";
const GRAPHICS_PACKS_STORAGE_KEY = "graphicsPacks";

export async function onPluginLoad() {
    if (graphicsPacks.kind !== "directory") throw new TypeError("Cemu graphics packs must be a directory");
    pluginStorage.setItem(
        GRAPHICS_PACKS_STORAGE_KEY,
        await loadGraphicsPacks(graphicsPacks),
    );
}

export function onConfigure(
    game: Package,
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
        groups,
        values,
    };
}

export async function onLaunch(args: EmulatorLaunchArgs<Options, Storages>) {
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

    args.filesystem.mount("/Cemu/Cemu.exe", cemuExecutable, { mode: "rx" });
    const discKeys: string[] = [];
    for (const key of await args.keyStore.list({ namespace: "content" })) {
        if (key.titleId !== undefined) continue;
        const bytes = await (await args.keyStore.open(key)).bytes();
        if (bytes.length !== 16) throw new Error(`Invalid Cemu disc key ${key.id}`);
        discKeys.push(Array.from(bytes, byte => byte.toString(16).padStart(2, "0")).join(""));
    }
    const keyFile = new TextEncoder().encode(discKeys.join("\n") + (discKeys.length ? "\n" : ""));
    args.filesystem.mount("/Cemu/keys.txt", keyFile, { mode: "r" });
    args.filesystem.mount("/Cemu/resources", resources, { mode: "rx" });
    args.filesystem.mount("/Cemu/gameProfiles", gameProfiles, { mode: "rx" });
    args.filesystem.mount("/Cemu/graphicPacks/downloadedGraphicPacks", graphicsPacks, { mode: "rx" });
    args.filesystem.mount("/Cemu/shaderCache", caches.shaderCache, { mode: "rwx" });
    for (const [name, bytes] of Object.entries(args.configuration.render())) {
        args.filesystem.mount(`/Cemu/${name}`, bytes, { mode: "rw" });
    }
    if (args.storage.mlc) {
        args.filesystem.mount("/Cemu/mlc01", args.storage.mlc, { mode: "rwx" });
    }
    const game = args.filesystem.mount("/Game", args.package, { mode: "rx" });
    args.filesystem.mount("/User");

    const executable = args.filesystem.path("/Cemu/Cemu.exe");
    const mlc = args.filesystem.path("/Cemu/mlc01");

    return {
        command: executable,
        workingDirectory: args.filesystem.path("/Cemu"),
        argv: ["-g", game, "--mlc", mlc],
        isolate: true,
        hooks: { graphics: [api], input: ["sdl3"] },
        surfaces: [
            { index: 0, pattern: "Cemu+", },
            { index: 1, pattern: "GamePad+", },
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
