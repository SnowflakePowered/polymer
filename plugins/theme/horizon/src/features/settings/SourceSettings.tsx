import { For, Show, createSignal, onCleanup } from "solid-js";
import type { LibrarySource } from "@polymer-fe/frontend";
import { Button, ConfirmDialog, IconButton, ListRow, Section } from "../../ui";
import { useShell } from "../../app/shell-context";
import "./SourceSettings.css";

const STATES: Record<LibrarySource["state"], string> = {
  watching: "Watching for changes",
  scanning: "Scanning…",
  offline: "Not reachable. Its games stay in the library until it returns.",
  idle: "Waiting",
};

export function SourceSettings() {
  const shell = useShell();
  const sources = window.Polymer.library.sources;
  const [list, setList] = createSignal<readonly LibrarySource[]>([]);
  const [adding, setAdding] = createSignal(false);
  const [removing, setRemoving] = createSignal<LibrarySource | null>(null);
  const [pending, setPending] = createSignal(false);
  const fail = (error: unknown) => shell.setModel("error", error instanceof Error ? error.message : String(error));

  void sources.list().then(setList, fail);
  const changed = (event: Event) => setList((event as CustomEvent<readonly LibrarySource[]>).detail);
  sources.addEventListener("change", changed);
  onCleanup(() => sources.removeEventListener("change", changed));

  const add = async () => {
    setAdding(true);
    try {
      await sources.add(await showDirectoryPicker());
      setList(await sources.list());
    } catch (error) {
      if (!(error instanceof DOMException && error.name === "AbortError")) fail(error);
    } finally {
      setAdding(false);
    }
  };
  const remove = async () => {
    const source = removing();
    if (!source) return;
    setPending(true);
    try {
      await sources.remove(source.id);
      setRemoving(null);
      setList(await sources.list());
    } catch (error) {
      fail(error);
    } finally {
      setPending(false);
    }
  };

  return <>
    <Section title="Add a folder" description="Games in library folders are added automatically and kept up to date as files are added or removed. Installed PC games and installable packages are skipped.">
      <div class="horizon-source-add">
        <Button appearance="primary" pending={adding()} onClick={() => void add()}>Browse…</Button>
      </div>
    </Section>
    <Section title="Library folders">
      <Show when={list().length} fallback={<p class="hz-description">No folders have been added.</p>}>
        <ul class="horizon-source-list">
          <For each={list()}>{source => <li>
            <ListRow label={source.root} description={STATES[source.state]}>
              <Button disabled={source.state !== "watching"} onClick={() => void sources.scan(source.id).catch(fail)}>Rescan</Button>
              <IconButton icon="trash" tone="danger" label={`Remove ${source.root}`} onClick={() => setRemoving(source)} />
            </ListRow>
          </li>}</For>
        </ul>
      </Show>
    </Section>
    <ConfirmDialog open={!!removing()} title="Remove folder?" pending={pending()} confirmLabel="Remove"
      description={`Games added from ${removing()?.root ?? ""} will be removed from the library. Games you imported yourself are kept, and no files are deleted.`}
      onConfirm={() => void remove()} onClose={() => setRemoving(null)} />
  </>;
}
