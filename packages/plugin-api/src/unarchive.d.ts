/**
 * Extract a zip, 7z, or DMG archive from a plugin cache into a cache directory as a
 * job. With `stripRoot`, the contents of a single top-level directory holding every entry
 * are extracted instead of the directory itself.
 */
export const unarchive: (
    file: import("polymer:api").PluginCacheFile,
    directory: import("polymer:api").PluginCacheDirectory,
    options?: { stripRoot?: boolean },
) => Promise<void>;
