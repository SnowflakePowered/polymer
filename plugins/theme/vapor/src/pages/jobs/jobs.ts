import { createSignal, onCleanup, onMount } from "solid-js";
import type { Job } from "../../../../../../js/polymer";

export const active = (job: Job) => job.state === "running";
export const pending = (job: Job) => job.state === "queued" || job.state === "paused";

export function useJobs() {
  const [jobs, setJobs] = createSignal<Job[]>([]);
  onMount(() => {
    const changed = (event: Event) => {
      const job = (event as CustomEvent<Job>).detail;
      setJobs(list => [job, ...list.filter(item => item.id !== job.id)].sort((a, b) => b.createdAt - a.createdAt));
    };
    window.Polymer.jobs.addEventListener("jobchange", changed);
    onCleanup(() => window.Polymer.jobs.removeEventListener("jobchange", changed));
    void window.Polymer.jobs.list().then(listed => setJobs(current => {
      const known = new Map(current.map(job => [job.id, job]));
      return [...listed.filter(job => !known.has(job.id)), ...current].sort((a, b) => b.createdAt - a.createdAt);
    })).catch(() => {});
  });
  return jobs;
}
