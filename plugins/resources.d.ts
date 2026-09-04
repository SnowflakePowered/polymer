declare module "resources:*" {
    const resource: import("./plugin-api.ts").DirectoryResource;
    export default resource;
}

declare module "polymer:pluginStorage" {
    export const pluginStorage: import("./plugin-api.ts").PluginStorage;
}

declare module "polymer:cache" {
    export const caches: Readonly<Record<string, import("./plugin-api.ts").PluginCache>>;
}

declare module "polymer:database" {
    export const connect: () => Promise<import("./plugin-api.ts").PluginDatabase>;
}
