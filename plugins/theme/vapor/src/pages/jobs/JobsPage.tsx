import { For, Show, createMemo, createSignal } from "solid-js";
import type { Job, JobKind } from "@polymer-fe/frontend";
import { useShell } from "../../app/shell";
import { Button, EmptyState, Field, Icon, Page, ProgressBar, type IconName } from "../../ui";
import { active, pending, useJobs } from "./jobs";
import "./jobs.css";

const KINDS: Record<JobKind, { label: string; icon: IconName }> = {
  scrape: { label: "Metadata", icon: "search" }, install: { label: "Install", icon: "storage" }, bootstrap: { label: "System setup", icon: "settings" },
  plugin: { label: "Plugin", icon: "info" }, import: { label: "Import", icon: "library" }, check: { label: "Update check", icon: "search" },
  update: { label: "Update", icon: "download" }, download: { label: "Download", icon: "download" }, refresh: { label: "Refresh", icon: "library" },
  scan: { label: "Folder scan", icon: "library" },
};
const STATES: Record<Job["state"], string> = { queued: "Queued", running: "In progress", paused: "Paused", completed: "Completed", partial: "Partially completed", failed: "Failed", cancelled: "Cancelled" };
const percent = (job: Job) => job.total ? Math.min(1, (job.completed ?? 0) / job.total) : undefined;
const when = (time: number) => new Date(time).toLocaleString(undefined, { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });

function JobIcon(props: { job: Job }) {
  return <Show when={props.job.icon} fallback={<Icon name={KINDS[props.job.kind].icon} color="#8b929a" />}>{icon => <img src={icon()} alt="" />}</Show>;
}

export function JobsPage() {
  const shell = useShell();
  const jobs = useJobs();
  const [busy, setBusy] = createSignal<string | null>(null);
  const top = createMemo(() => jobs().filter(job => !job.parent));
  const children = (job: Job) => jobs().filter(child => child.parent === job.id);
  const running = () => top().filter(active), queued = () => top().filter(pending), finished = () => top().filter(job => !active(job) && !pending(job));
  const act = (job: Job, action: (id: string) => Promise<unknown>) => {
    setBusy(job.id);
    void action(job.id).catch(shell.fail).finally(() => setBusy(null));
  };
  const describe = (job: Job) => [KINDS[job.kind].label, job.error ?? job.message, job.subject.platform && window.Polymer.machine(job.subject.platform)?.name,
    children(job).length ? `${children(job).filter(child => !active(child) && !pending(child)).length} of ${children(job).length} steps` : null].filter(Boolean).join(" · ");
  return <Page class="vp-jobs" focusWhen={() => top().length} hints={[{ action: "activate", label: "Select" }, { action: "back", label: "Back" }]} onAction={action => {
    if (action.type !== "back") return false;
    shell.navigate("/");
    return true;
  }}>
    <div class="vp-jobs-body">
      <h1 class="vp-jobs-title">Downloads</h1>
      <Show when={top().length} fallback={<EmptyState title="Nothing in progress" body="Installs, downloads and library imports appear here." />}>
        <Show when={running().length}>
          <h2 class="vp-section-title">In progress</h2>
          <For each={running()}>{job => <Field label={job.title} description={describe(job)} icon={<JobIcon job={job} />} childrenBelow>
            <ProgressBar value={percent(job)} label={STATES[job.state]} detail={percent(job) === undefined ? undefined : `${Math.round(percent(job)! * 100)}%`} />
          </Field>}</For>
        </Show>
        <Show when={queued().length}>
          <h2 class="vp-section-title">Up next</h2>
          <For each={queued()}>{job => <Field label={job.title} description={`${STATES[job.state]} · ${describe(job)}`} icon={<JobIcon job={job} />}>
            <Button disabled={busy() === job.id} onClick={() => act(job, id => window.Polymer.jobs.cancel(id))}>Cancel</Button>
          </Field>}</For>
        </Show>
        <Show when={finished().length}>
          <h2 class="vp-section-title">Completed</h2>
          <For each={finished()}>{job => <Field label={job.title} description={`${STATES[job.state]} ${when(job.updatedAt)} · ${describe(job)}`} icon={<JobIcon job={job} />}>
            <Show when={job.state !== "completed"}><Button disabled={busy() === job.id} onClick={() => act(job, id => window.Polymer.jobs.retry(id))}>Retry</Button></Show>
            <Button disabled={busy() === job.id} onClick={() => act(job, id => window.Polymer.jobs.dismiss(id))}>Clear</Button>
          </Field>}</For>
        </Show>
      </Show>
    </div>
  </Page>;
}
