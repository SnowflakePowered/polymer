/** Opens an independent connection. Cache files default to read-only; the
 * plugin's persistent database (no file argument) defaults to writable.
 * Set readOnly: false to create or update a database within a declared cache.
 */
export const connect: (
    file?: import("polymer:api").PluginCacheFile,
    options?: { readOnly?: boolean },
) => Promise<import("polymer:api").PluginDatabase>;
