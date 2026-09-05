declare module "pkg:*" {
    const resource: import("polymer:api").DirectoryResource;
    export default resource;
}

declare module "polymer:pluginStorage" {
    export const pluginStorage: import("polymer:api").PluginStorage;
}

declare module "polymer:cache" {
    export const caches: Readonly<Record<string, import("polymer:api").PluginCache>>;
}

declare module "polymer:database" {
    export const connect: () => Promise<import("polymer:api").PluginDatabase>;
}

declare module "polymer:uiautomation" {
    export const uiAutomation: import("polymer:api").UiAutomation;
}

declare module "polymer:actions" {
    export const actions: import("polymer:api").Actions;
}
