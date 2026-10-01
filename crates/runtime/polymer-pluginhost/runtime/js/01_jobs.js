import { core } from "ext:core/mod.js";
import { op_job_finish, op_job_report, op_job_start } from "ext:core/ops";

const ambient = new core.AsyncVariable();
const HOSTED = Symbol("hosted");
let ridOf;

function validProgress({ message = null, completed = null, total = null }) {
  if ((message !== null && typeof message !== "string") ||
      (completed !== null && (!Number.isFinite(completed) || completed < 0)) ||
      (total !== null && (!Number.isFinite(total) || total <= 0)) ||
      (completed !== null && total !== null && completed > total)) {
    throw new TypeError("invalid job progress");
  }
  return { message, completed, total };
}

// Registered as soon as it is constructed, under the job whose `run` scope it was made in.
// Awaiting a job waits for it to finish.
class Job {
  #rid;
  #reports;
  #finished = false;
  #done;
  #settle;

  constructor(title, rid) {
    this.#done = new Promise((resolve, reject) => { this.#settle = { resolve, reject }; });
    this.#done.catch(() => {});
    if (title === HOSTED) {
      this.#rid = this.#reports = Promise.resolve(rid);
      this.#finished = true;
      return;
    }
    if (typeof title !== "string" || !title) throw new TypeError("a job needs a title");
    this.#rid = ambientJob().then(parent => op_job_start(title, parent));
    this.#reports = this.#rid;
  }

  static {
    ridOf = job => job.#rid;
  }

  progress(update) {
    if (this.#finished) throw new TypeError("job has already finished");
    const { message, completed, total } = validProgress(update ?? {});
    this.#reports = this.#reports.then(rid => {
      if (rid !== null) op_job_report(rid, message, completed, total);
      return rid;
    });
  }

  run(work) {
    const previous = ambient.enter(this);
    let result;
    try {
      result = new Promise(resolve => resolve(work(this)));
    } finally {
      core.setAsyncContext(previous);
    }
    return result.then(
      async value => { await this.complete(); return value; },
      async error => { await this.fail(error); throw error; },
    );
  }

  complete() {
    return this.#finish(null);
  }

  fail(error) {
    return this.#finish(String(error?.message ?? error ?? "Failed"), error);
  }

  then(fulfilled, rejected) {
    return this.#done.then(fulfilled, rejected);
  }

  #finish(message, error) {
    if (this.#finished) throw new TypeError("job has already finished");
    this.#finished = true;
    const finished = this.#reports.then(rid => rid === null ? undefined : op_job_finish(rid, message));
    finished.then(
      () => message === null ? this.#settle.resolve() : this.#settle.reject(error ?? new Error(message)),
      this.#settle.reject,
    );
    return finished;
  }
}

const TRACKED_REPORT_BYTES = 1024 * 1024;

// Reports the bytes read through `source` to the job of the `run` scope it was made in.
class JobTrackedStream {
  constructor(source, { message = null, total = null, maxByteLength = null } = {}) {
    if (!(total > 0)) total = null;
    validProgress({ message, total });
    const parent = ambientJob();
    const reader = source.getReader();
    let completed = 0;
    let reported = 0;
    const report = () => {
      reported = completed;
      const known = total === null ? null : Math.max(total, completed);
      parent.then(rid => { if (rid !== null) op_job_report(rid, message, completed, known); });
    };
    return new ReadableStream({
      start: report,
      async pull(controller) {
        const { done, value } = await reader.read();
        if (done) {
          report();
          controller.close();
          return;
        }
        completed += value.byteLength;
        if (maxByteLength !== null && completed > maxByteLength) {
          const error = new RangeError(`${message ?? "stream"} exceeds ${maxByteLength} bytes`);
          await reader.cancel(error);
          throw error;
        }
        if (completed - reported >= TRACKED_REPORT_BYTES) report();
        controller.enqueue(value);
      },
      cancel: reason => reader.cancel(reason),
    });
  }
}

export function ambientJob() {
  const parent = ambient.get();
  return parent ? ridOf(parent) : Promise.resolve(null);
}
Object.freeze(Job.prototype);

// Runs an invocation with the host's job, if any, as the root scope of every job it starts.
export function hosted(rid, work) {
  if (rid === null) return work();
  const previous = ambient.enter(new Job(HOSTED, rid));
  try {
    return work();
  } finally {
    core.setAsyncContext(previous);
  }
}

Object.defineProperty(globalThis, "__polymerJobs", {
  value: Object.freeze({ Job: Object.freeze(Job), JobTrackedStream: Object.freeze(JobTrackedStream) }),
});
