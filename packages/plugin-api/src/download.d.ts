/**
 * Download `url` into a cache file as a persistent `download` job, verifying its size
 * and digest. An interrupted download resumes the next time the file is requested, and
 * a download of the same file already in progress is shared.
 */
export const download: (
    url: string,
    file: import("polymer:api").PluginCacheFile,
    options?: import("polymer:api").DownloadOptions,
) => Promise<void>;
