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

declare module "polymer:download" {
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
}

declare module "polymer:jobs" {
    /**
     * An in-memory job in the job queue for long-running work. The job is queued as soon
     * as it is constructed, under the job whose `run` scope constructed it, and runs until
     * the plugin finishes it; a job left unfinished fails when the plugin's invocation ends.
     * Awaiting a job waits for it to finish.
     */
    export class Job implements PromiseLike<void> {
        constructor(title: string);
        progress(update: import("polymer:api").JobProgress): void;
        /**
         * Runs `work` with this job as the parent of every job, download and install started
         * in it, across awaits. The job completes or fails with `work`.
         */
        run<T>(work: (job: Job) => T | PromiseLike<T>): Promise<T>;
        complete(): Promise<void>;
        fail(error?: unknown): Promise<void>;
        then<TResult1 = void, TResult2 = never>(
            onfulfilled?: ((value: void) => TResult1 | PromiseLike<TResult1>) | null,
            onrejected?: ((reason: unknown) => TResult2 | PromiseLike<TResult2>) | null,
        ): PromiseLike<TResult1 | TResult2>;
    }

    /**
     * Wraps `source` in a stream that reports the bytes read through it as `completed`
     * progress, with `message` and `total`, to the job of the `run` scope it is made in.
     * The stream errors once more than `maxByteLength` bytes are read.
     */
    export const JobTrackedStream: new (
        source: ReadableStream<Uint8Array>,
        options?: { message?: string; total?: number; maxByteLength?: number },
    ) => ReadableStream<Uint8Array>;
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
