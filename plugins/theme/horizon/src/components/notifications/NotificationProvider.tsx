import { createEffect, onCleanup, onMount, untrack, type ParentProps } from "solid-js";
import type { Job } from "../../../../../../js/polymer";
import { useShell } from "../../app/shell-context";
import { NotificationContext } from "./notification-context";
import { NotificationStore } from "./NotificationStore";

export function NotificationProvider(props: ParentProps) {
  const shell = useShell();
  const notifications = new NotificationStore();
  onCleanup(() => notifications.dispose());
  createEffect(() => {
    const activities = shell.model.libraryActivities.map(activity => ({ ...activity }));
    untrack(() => {
      for (const activity of activities) notifications.upsert({ ...activity, id: `library:${activity.id}` });
      if (!activities.some(item => item.id === "imports")) notifications.remove("library:imports");
    });
  });
  createEffect(() => {
    const message = shell.model.error;
    untrack(() => message ? notifications.upsert({ id: "error", title: "Something went wrong", message, state: "error" }) : notifications.remove("error"));
  });
  createEffect(() => {
    const toast = shell.model.controllerToast;
    const name = toast?.name, index = toast?.index;
    untrack(() => name !== undefined ? notifications.upsert({ id: "controller", title: name, message: `Connected as player ${index! + 1}`, state: "info" }) : notifications.remove("controller"));
  });
  onMount(() => {
    // Scrape and import jobs are announced through the library activities above.
    const receive = (job: Job) => {
      if (job.kind === "scrape" || job.kind === "import") return;
      const id = `job:${job.id}`;
      if (job.kind === "plugin" && job.state === "completed") return notifications.remove(id);
      const active = job.state === "queued" || job.state === "running" || job.state === "paused";
      const plugin = job.subject.pluginId;
      notifications.upsert({ id, title: plugin ? window.Polymer.machine(plugin)?.name ?? job.title : job.title, icon: job.icon ?? undefined,
        message: job.error ?? job.message ?? "Waiting",
        state: active ? "running" : job.state === "completed" ? "complete" : "error",
        completed: active ? job.completed ?? undefined : undefined, total: active ? job.total ?? undefined : undefined });
    };
    const listener = (event: Event) => receive((event as CustomEvent<Job>).detail);
    window.Polymer.jobs.addEventListener("jobchange", listener);
    // Late listeners restore running work, not historical completion notices.
    void window.Polymer.jobs.list().then(jobs => jobs.filter(job => job.state === "queued" || job.state === "running").forEach(receive));
    onCleanup(() => window.Polymer.jobs.removeEventListener("jobchange", listener));
  });
  return <NotificationContext.Provider value={notifications}>{props.children}</NotificationContext.Provider>;
}
