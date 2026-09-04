declare module "resources:*" {
    const resource: import("./plugin-api.ts").DirectoryResource;
    export default resource;
}

declare module "snowglass:pluginStorage" {
    export const pluginStorage: import("./plugin-api.ts").PluginStorage;
}

declare module "snowglass:cache" {
    export const caches: Readonly<Record<string, import("./plugin-api.ts").PluginCache>>;
}

declare module "snowglass:database" {
    export const connect: () => Promise<import("./plugin-api.ts").PluginDatabase>;
}
