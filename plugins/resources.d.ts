declare module "pkg:*" {
    const resource: import("polymer:api").DirEntry;
    export default resource;
}

declare module "polymer:pluginStorage" {
    export const pluginStorage: import("polymer:api").PluginStorage;
}

declare module "polymer:cache" {
    export const caches: Readonly<Record<string, import("polymer:api").PluginCache>>;
}

declare module "polymer:database" {
    /** Opens an independent connection. Cache files default to read-only; the
     * plugin's persistent database (no file argument) defaults to writable.
     * Set readOnly: false to create or update a database within a declared cache.
     */
    export const connect: (
        file?: import("polymer:api").PluginCacheFile,
        options?: { readOnly?: boolean },
    ) => Promise<import("polymer:api").PluginDatabase>;
}

declare module "polymer:uiautomation" {
    export const uiAutomation: import("polymer:api").UiAutomation;
}
