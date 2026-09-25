import { createEffect, onCleanup, onMount, untrack, type ParentProps } from "solid-js";
import type { PluginEvent, PluginLoadProgress, StorageInstall } from "../../../../../../js/polymer";
import { useShell } from "../../app/shell-context";
import { NotificationContext } from "./notification-context";
import { NotificationStore } from "./NotificationStore";
import { installPhases } from "../../features/storage/selection";

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
    const receive = (event: PluginEvent) => {
      if (event.name !== "loadprogress") return;
      const load = event.detail as Omit<PluginLoadProgress, "id" | "pluginId">;
      notifications.upsert({ id: `plugin:${event.sourceId}`, title: window.Polymer.machine(event.pluginId)?.name ?? event.pluginId, message: load.message,
        state: load.state === "ready" ? "complete" : load.state === "failed" ? "error" : "running",
        completed: load.completed ?? undefined, total: load.total ?? undefined });
    };
    const listener = (event: Event) => receive((event as CustomEvent<PluginEvent>).detail);
    window.Polymer.plugin.addEventListener("loadprogress", listener);
    // Late listeners restore running work, not historical completion notices.
    window.Polymer.plugin.events().filter(event => event.name === "loadprogress"
      && (event.detail as PluginLoadProgress).state === "loading").forEach(receive);
    onCleanup(() => window.Polymer.plugin.removeEventListener("loadprogress", listener));
  });
  onMount(() => {
    const titles = new Map<string, { title: string; icon?: string }>();
    const receive = (install: StorageInstall) => {
      const known = titles.get(install.id) ?? { title: install.filename };
      const title = install.state === "running" && install.detail.title ? { title: install.detail.title.label, icon: install.detail.title.icon ?? undefined }
        : install.state === "complete" ? { title: install.detail.label, icon: install.detail.icon ?? known.icon } : known;
      titles.set(install.id, title);
      if (install.state === "complete" || install.state === "failed") titles.delete(install.id);
      notifications.upsert({ id: `install:${install.id}`, ...title,
        message: install.state === "queued" ? "Waiting to install" : install.state === "running" ? installPhases[install.detail.phase] : install.state === "complete" ? "Installed" : install.detail,
        state: install.state === "complete" ? "complete" : install.state === "failed" ? "error" : "running",
        completed: install.state === "running" && install.detail.total ? install.detail.completed : undefined,
        total: install.state === "running" && install.detail.total ? install.detail.total : undefined });
    };
    const listener = (event: Event) => receive((event as CustomEvent<StorageInstall>).detail);
    window.Polymer.consolestorage.addEventListener("installchange", listener);
    window.Polymer.consolestorage.installs.forEach(receive);
    onCleanup(() => window.Polymer.consolestorage.removeEventListener("installchange", listener));
  });
  return <NotificationContext.Provider value={notifications}>{props.children}</NotificationContext.Provider>;
}
