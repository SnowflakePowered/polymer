import { op_jobs_cancel, op_jobs_dismiss, op_jobs_get, op_jobs_list, op_jobs_retry } from "ext:core/ops";

export function createJobs() {
  const Jobs = new EventTarget();
  Object.defineProperties(Jobs, {
    list: { value: () => op_jobs_list() },
    get: { value: id => op_jobs_get(String(id)) },
    cancel: { value: id => op_jobs_cancel(String(id)) },
    retry: { value: id => op_jobs_retry(String(id)) },
    dismiss: { value: id => op_jobs_dismiss(String(id)) },
  });
  return Object.freeze({
    Jobs,
    changed(job) {
      Jobs.dispatchEvent(new CustomEvent("jobchange", { detail: Object.freeze(job) }));
    },
  });
}
