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
