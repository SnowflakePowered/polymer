import { For, onCleanup, onMount } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import type { PluginEvent, PluginLoadProgress as LoadProgress } from "../../../../../js/polymer";
import "./PluginLoadProgress.css";

export function PluginLoadProgress() {
  const [loads, setLoads] = createStore<LoadProgress[]>([]);
  onMount(() => {
    const receive = (event: PluginEvent) => {
      if (event.name !== "loadprogress") return;
      const detail = event.detail as Omit<LoadProgress, "id" | "pluginId">;
      const remaining = loads.filter(load => load.id !== event.sourceId);
      if (detail.state !== "ready") remaining.push({
        ...detail, id: event.sourceId, pluginId: event.pluginId,
        completed: detail.completed ?? null, total: detail.total ?? null,
      });
      setLoads(reconcile(remaining, { key: "id" }));
    };
    const listener = (event: Event) => receive((event as CustomEvent<PluginEvent>).detail);
    window.Polymer.plugin.addEventListener("loadprogress", listener);
    window.Polymer.plugin.events().forEach(receive);
    onCleanup(() => window.Polymer.plugin.removeEventListener("loadprogress", listener));
  });
  return <aside class="horizon-plugin-loads" aria-live="polite">
    <For each={loads}>{load => <section class="horizon-plugin-load" role={load.state === "failed" ? "alert" : "status"}>
      <strong>{load.pluginId}</strong><span>{load.message}</span>
      {load.state === "loading" && <div class="horizon-plugin-load-track" role="progressbar" aria-label={`${load.pluginId}: ${load.message}`} aria-valuemin={0} aria-valuenow={load.total && load.completed !== null ? load.completed : undefined} aria-valuemax={load.total || undefined}>
        <div class="horizon-plugin-load-fill" classList={{ indeterminate: !load.total || load.completed === null }} style={{ width: load.total && load.completed !== null ? `${Math.min(100, 100 * load.completed / load.total)}%` : "40%" }} />
      </div>}
    </section>}</For>
  </aside>;
}
