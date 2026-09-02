declare module "resources:*" {
    const resource: import("./plugin-api.ts").DirectoryResource;
    export default resource;
}

declare module "snowglass:pluginStorage" {
    export const pluginStorage: import("./plugin-api.ts").PluginStorage;
}
