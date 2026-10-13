import { Index, Show, createSignal, onCleanup } from "solid-js";
import type { SaveState, SaveStateStore } from "@polymer-fe/frontend";
import { SaveStateSlots } from "@polymer-fe/frontend/save-states";
import "./SuspendPoints.css";

export function SuspendPoints(props: { store: SaveStateStore; mode: "load" | "save"; onRestored(): void }) {
  const [entries, setEntries] = createSignal(props.store.entries);
  const [pending, setPending] = createSignal<number | null>(null);
  const [error, setError] = createSignal("");
  const changed = (event: Event) => setEntries((event as CustomEvent<readonly SaveState[]>).detail);
  props.store.addEventListener("change", changed);
  onCleanup(() => props.store.removeEventListener("change", changed));
  const slots = () => { entries(); return SaveStateSlots.slots(props.store); };
  const run = async (slot: number) => {
    if (pending() !== null) return;
    setPending(slot);
    setError("");
    try {
      if (props.mode === "save") await SaveStateSlots.save(props.store, slot);
      else { await props.store.restore(slot); props.onRestored(); }
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setPending(null);
    }
  };
  return <div class="horizon-suspend">
    <h2 class="horizon-suspend-heading">{props.mode === "save" ? "Create Suspend Point" : "Load Suspend Point"}</h2>
    <p class="horizon-suspend-warning">⚠️ Save states are lost when the game is closed</p>
    <Show when={error()}><p class="horizon-suspend-error" role="alert">{error()}</p></Show>
    <div class="horizon-suspend-slots" role="list">
      <Index each={slots()}>{item =>
        <button type="button" role="listitem" class="horizon-suspend-slot" data-select-ring disabled={props.mode === "load" && !item().entry} aria-busy={pending() === item().slot || undefined} onClick={() => void run(item().slot)}>
          <span class="horizon-suspend-thumb">
            <Show when={item().entry && SaveStateSlots.screenshot(item().entry!)} fallback={<span>{item().entry ? "No image." : "No data."}</span>}>{url => <img src={url()} alt="" />}</Show>
          </span>
          <span class="horizon-suspend-label">
            <span>{item().entry?.label ?? `Save slot ${item().slot + 1}`}</span>
            <Show when={pending() === item().slot} fallback={<Show when={item().entry}>{saved => <small>{saved().createdAt.toLocaleDateString()} - {saved().createdAt.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</small>}</Show>}>
              <small>{props.mode === "save" ? "Saving…" : "Loading…"}</small>
            </Show>
          </span>
        </button>
      }</Index>
    </div>
  </div>;
}
