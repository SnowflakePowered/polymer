import { Index, Show, createEffect, createSignal, onCleanup } from "solid-js";
import type { SaveState, SaveStateStore } from "@polymer-fe/frontend";
import { SaveStateSlots } from "@polymer-fe/frontend/save-states";
import { useShell } from "../../app/shell";
import { Button, Modal } from "../../ui";

function SaveStates(props: { store: SaveStateStore; onDone(): void; onRestored(): void }) {
  const shell = useShell();
  const [entries, setEntries] = createSignal(props.store.entries);
  const [pending, setPending] = createSignal<number | null>(null);
  const changed = (event: Event) => setEntries((event as CustomEvent<readonly SaveState[]>).detail);
  props.store.addEventListener("change", changed);
  onCleanup(() => props.store.removeEventListener("change", changed));
  const slots = () => { entries(); return SaveStateSlots.slots(props.store); };
  const run = (slot: number, task: () => Promise<unknown>) => {
    if (pending() !== null) return;
    setPending(slot);
    void task().catch(shell.fail).finally(() => setPending(null));
  };
  const when = (entry: SaveState) => entry.createdAt.toLocaleString([], { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });
  return <Modal open class="vp-save-states" onClose={props.onDone} title="Save states" initialFocus={() => document.querySelector<HTMLElement>(".vp-save-state .vp-button")}
    buttons={<Button primary onClick={props.onDone}>Done</Button>}>
    <p class="vp-save-state-warning">⚠️ Save states are lost when the game is closed</p>
    <div class="vp-save-state-list">
      <Index each={slots()}>{item =>
        <div class="vp-save-state" data-empty={!item().entry || undefined}>
          <div class="vp-save-state-thumb">
            <Show when={item().entry && SaveStateSlots.screenshot(item().entry!)} fallback={<span>{item().entry ? "No preview" : "Empty"}</span>}>{url => <img src={url()} alt="" />}</Show>
          </div>
          <div class="vp-save-state-info">
            <span class="vp-save-state-name">{item().entry?.label ?? `Slot ${item().slot + 1}`}</span>
            <small>{pending() === item().slot ? "Working…" : item().entry ? when(item().entry!) : "No save in this slot"}</small>
          </div>
          <div class="vp-save-state-actions">
            <Button onClick={() => run(item().slot, () => SaveStateSlots.save(props.store, item().slot))}>{item().entry ? "Overwrite" : "Save"}</Button>
            <Button primary disabled={!item().entry} onClick={() => run(item().slot, () => props.store.restore(item().slot).then(props.onRestored))}>Load</Button>
          </div>
        </div>
      }</Index>
    </div>
  </Modal>;
}

export function SaveStatesModal() {
  const shell = useShell();
  const store = () => shell.state.menu === "states" ? shell.state.launch?.saveStates : null;
  createEffect(() => {
    const launch = shell.state.launch;
    if (!launch || !store()) return;
    const release = shell.pauses.acquire(launch);
    onCleanup(() => void release().catch(error => console.error("Failed to resume emulator", error)));
  });
  return <Show when={store()} keyed>{current => <SaveStates store={current} onDone={() => shell.update("menu", "quick")} onRestored={() => shell.update("menu", null)} />}</Show>;
}
