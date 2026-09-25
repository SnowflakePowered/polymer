import { createEffect, createMemo, createSignal, For, Show } from "solid-js";
import { useNotifications } from "./notification-context";
import { SidebarJob } from "./SidebarJob";
import "./SidebarJobs.css";
export function SidebarJobs() {
  const notifications = useNotifications();
  const jobs = createMemo(() => notifications.items().filter(item => item.dismissed && item.state === "running"));
  const [selectedId, setSelectedId] = createSignal<string>();
  const selected = createMemo(() => jobs().find(item => item.id === selectedId()) ?? jobs()[0]);
  createEffect(() => setSelectedId(selected()?.id));
  // Keep the dot strip within one icon even when many jobs are running.
  const pages = createMemo(() => {
    const index = jobs().findIndex(item => item.id === selected()?.id);
    const start = Math.max(0, Math.min(index - 2, jobs().length - 5));
    return jobs().slice(start, start + 5);
  });
  return <Show when={selected()}>{item =>
    <div class="horizon-sidebar-jobs" aria-label="Background jobs">
      <SidebarJob notification={item()} onOpen={() => notifications.reopen(item().id)} />
      <Show when={jobs().length > 1}>
        <div class="horizon-sidebar-job-pages" role="group" aria-label="Select background job">
          <For each={pages()}>{page =>
            <button type="button" class="horizon-sidebar-job-page"
              aria-label={`${page.title}: job ${jobs().indexOf(page) + 1} of ${jobs().length}`}
              aria-pressed={page.id === item().id} title={`${page.title}: ${page.message}`}
              onClick={() => setSelectedId(page.id)} />
          }</For>
        </div>
      </Show>
    </div>
  }</Show>;
}
