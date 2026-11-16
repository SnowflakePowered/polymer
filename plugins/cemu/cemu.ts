import { caches, config, pluginStorage } from "polymer:plugin";
import { connect } from "polymer:database";
import { download } from "polymer:download";
import { Job } from "polymer:jobs";
import { graphicsApis, osIdentifier } from "polymer:sys";
import { unarchive } from "polymer:unarchive";
import { EmulatorUpdates } from "jsr:@polymer-fe/emulator-updates@^0.1.0";
import type {
    ConfigurationResult,
    DirEntry,
    DirectoryEntry,
    EmulatorLaunchArgs,
    Package,
    Storage,
    RepeatedSection,
    SaveCheckpointContext,
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
    "content-graphic": { api: number, async_compile: boolean, vsync: number },
};

type Storages = Readonly<{
    slc?: Storage,
    slccmpt?: Storage,
    mlc?: Storage,
    sd?: Storage,
}>;

const GRAPHICS_GROUP = "graphics";
const GRAPHICS_PACKS_RELEASE = "https://api.github.com/repos/cemu-project/cemu_graphic_packs/releases/latest";

const runtime = new EmulatorUpdates({
    name: "Cemu",
    cache: caches.runtime,
    osIdentifier,
    channel: String(config.channel ?? "stable"),
    connect,
    download,
    unarchive,
    Job,
});

const graphicsPacks = new EmulatorUpdates({
    name: "Cemu graphics packs",
    cache: caches.graphicsPacks,
    table: "graphics_pack_generations",
    osIdentifier,
    connect,
    download,
    unarchive,
    Job,
});

export const onPluginLoad = () => runtime.ready();

export async function onUpdateContents() {
    await runtime.update([await runtime.release("cemu")]);
    const response = await fetch(GRAPHICS_PACKS_RELEASE);
    if (!response.ok) throw new Error(`Cemu graphics packs request failed with ${response.status}`);
    const release = await response.json();
    const asset = release.assets?.find((asset: { name: string }) => /^graphicPacks.*\.zip$/.test(asset.name));
    if (!asset?.digest?.startsWith("sha256:")) throw new Error("Cemu graphics packs release has no verifiable archive");
    await graphicsPacks.update([{
        version: release.tag_name,
        url: asset.browser_download_url,
        sha256: asset.digest.slice("sha256:".length),
        size: asset.size,
    }]);
}

export async function onConfigure(
    game: Package,
    userConfiguration?: UserConfiguration,
): Promise<ConfigurationResult> {
    const api = selectedApi(userConfiguration);
    const groups: Array<ConfigurationResult["groups"][number]> = [{
        key: GRAPHICS_GROUP,
        title: "Graphics Options",
        options: [{
            key: "api",
            title: "Graphics API",
            type: "select",
            default: GRAPHICS_APIS[0]?.key ?? "vulkan",
            choices: GRAPHICS_APIS,
        }, {
            key: "asyncShaders",
            title: "Async Shader Compile",
            description: "Compile shaders in the background to reduce stutter. Vulkan only.",
            type: "bool",
            default: true,
        }, {
            key: "vsync",
            title: "VSync",
            description: "Synchronize frames to the display's refresh rate to prevent tearing.",
            type: "bool",
            default: false,
        }],
    }];
    const values: Record<string, Record<string, boolean | number | string | null>> = {
        [GRAPHICS_GROUP]: { api, asyncShaders: asyncShaders(userConfiguration), vsync: userConfiguration?.[GRAPHICS_GROUP]?.vsync === true },
    };
    const generation = await graphicsPacks.active();
    if (normalizeTitleId(game.titleId) && generation) {
        const packs = await loadedGraphicsPacks(generation);
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
    args.configuration.options()["content-graphic"].async_compile = asyncShaders(userConfiguration);
    args.configuration.options()["content-graphic"].vsync = userConfiguration[GRAPHICS_GROUP]?.vsync === true ? 1 : 0;

    const generation = await runtime.launch();
    const entries = await runtimeEntries(generation);
    const packsGeneration = await graphicsPacks.ready() ? await graphicsPacks.launch() : undefined;
    const packs = packsGeneration && normalizeTitleId(args.launchSet[0].package.titleId) ? await loadedGraphicsPacks(packsGeneration) : [];
    const selected = settleGraphicsPacks(
        packs,
        args.launchSet[0].package.titleId,
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

    args.filesystem.mount("/Cemu/Cemu.exe", entries.get("Cemu.exe")!, { mode: "rx" });
    const discKeys: string[] = [];
    for (const key of await args.keyStore.list({ namespace: "content" })) {
        if (key.titleId !== undefined) continue;
        const bytes = await (await args.keyStore.open(key)).bytes();
        if (bytes.length !== 16) throw new Error(`Invalid Cemu disc key ${key.id}`);
        discKeys.push(Array.from(bytes, byte => byte.toString(16).padStart(2, "0")).join(""));
    }
    const keyFile = new TextEncoder().encode(discKeys.join("\n") + (discKeys.length ? "\n" : ""));
    args.filesystem.mount("/Cemu/keys.txt", keyFile, { mode: "r" });
    args.filesystem.mount("/Cemu/resources", entries.get("resources")!, { mode: "rx" });
    args.filesystem.mount("/Cemu/gameProfiles", entries.get("gameProfiles")!, { mode: "rx" });
    if (packsGeneration) args.filesystem.mount("/Cemu/graphicPacks/downloadedGraphicPacks", packsGeneration, { mode: "rx" });
    args.filesystem.mount("/Cemu/shaderCache", caches.shaderCache, { mode: "rwx" });
    for (const [name, bytes] of Object.entries(args.configuration.render())) {
        args.filesystem.mount(`/Cemu/${name}`, bytes, { mode: "rw" });
    }
    if (args.storage.mlc) {
        args.filesystem.mount("/Cemu/mlc01", args.storage.mlc, { mode: "rwx" });
    }
    const game = args.filesystem.mount("/Game", args.launchSet[0].package, { mode: "rx" });
    args.filesystem.mount("/User");

    const executable = args.filesystem.path("/Cemu/Cemu.exe");
    const mlc = args.filesystem.path("/Cemu/mlc01");

    return {
        command: executable,
        workingDirectory: args.filesystem.path("/Cemu"),
        env: [
            ["USERPROFILE", args.filesystem.path("/User")],
            ["APPDATA", args.filesystem.path("/User/AppData/Roaming")],
            ["LOCALAPPDATA", args.filesystem.path("/User/AppData/Local")],
            ["TEMP", args.filesystem.path("/User/Temp")],
            ["TMP", args.filesystem.path("/User/Temp")],
        ],
        argv: ["-g", game, "--mlc", mlc],
        isolate: true,
        hooks: { graphics: [api], input: ["sdl3"] },
        surfaces: [
            { index: 0, pattern: "Cemu+", },
            { index: 1, pattern: "GamePad+", },
        ],
    };
}

const GRAPHICS_APIS = ([
    { key: "vulkan", label: "Vulkan" },
    { key: "opengl", label: "OpenGL" },
] satisfies { key: GraphicsApi, label: string }[]).filter(api => graphicsApis.includes(api.key));

function asyncShaders(configuration: UserConfiguration | undefined): boolean {
    return configuration?.[GRAPHICS_GROUP]?.asyncShaders !== false;
}

function selectedApi(configuration: UserConfiguration | undefined): GraphicsApi {
    const api = configuration?.[GRAPHICS_GROUP]?.api;
    return GRAPHICS_APIS.find(choice => choice.key === api)?.key ?? GRAPHICS_APIS[0]?.key ?? "vulkan";
}

async function runtimeEntries(generation: DirectoryEntry): Promise<Map<string, DirEntry>> {
    const entries = new Map<string, DirEntry>();
    for await (const entry of generation) entries.set(entry.name, entry);
    return entries;
}

async function loadedGraphicsPacks(generation: DirectoryEntry): Promise<GraphicsPack[]> {
    const key = `graphicsPacks:${generation.name}`;
    const cached = pluginStorage.getItem<GraphicsPack[]>(key);
    if (cached !== null) return cached;
    const packs = await loadGraphicsPacks(generation);
    pluginStorage.setItem(key, packs);
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

export function onSaveCheckpoint({ storage, save, changed }: SaveCheckpointContext) {
    if (storage !== "mlc") return true;
    const user = `${save.path}/user/`;
    const committed = `${save.path}/meta/saveinfo.xml`;
    return !changed.some((entry) => entry.path.startsWith(user))
        || changed.some((entry) => entry.path === committed);
}

export default { onPluginLoad, onUpdateContents, onConfigure, onLaunch, onSaveCheckpoint };
