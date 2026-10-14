/** The caches declared in `[[plugin.caches]]`, by key. */
export const caches: Readonly<Record<string, import("polymer:api").PluginCache>>;

/**
 * The plugin's `[plugin.config]` values. Options with a `display` name hold the user's
 * choice, or their `default`; other keys hold their value from the manifest. Values are
 * read when the invocation starts.
 */
export const config: Readonly<Record<string, unknown>>;

/** The plugin's parsed `plugin.toml`. */
export const manifest: Readonly<Record<string, any>>;

/**
 * In-memory store shared by every isolate of the plugin in this process, holding
 * JSON-compatible values. It is not persisted.
 */
export const pluginStorage: import("polymer:api").PluginStorage;
