import { For, Show, createEffect, createMemo, createResource, createSignal, onCleanup } from "solid-js";
import { Section, DetailList, Icon, IconButton, ListRow, Select, TextField, ConfirmDialog } from "../../ui";
import type { ImportedKey } from "../../../../../../js/polymer";
import { useShell } from "../../app/shell-context";
import "./KeySettings.css";

const VISIBLE_KEYS = 100;

export function KeySettings() {
  const shell = useShell();
  const [dropActive, setDropActive] = createSignal(window.Polymer.scraping.dropActive);
  const [platform, setPlatform] = createSignal<string | null>(null);
  const [filter, setFilter] = createSignal("");
  const [removing, setRemoving] = createSignal<ImportedKey | null>(null);
  const [pending, setPending] = createSignal(false);
  const [stored, { refetch: refetchPlatforms }] = createResource(() => window.Polymer.library.keys.platforms());
  const platforms = createMemo(() => [...new Set([
    ...(stored() ?? []),
    ...shell.model.libraryGames.filter(game => game.missingKey).map(game => game.platform),
  ])].sort().map(value => ({ value, label: window.Polymer.machine(value)?.name ?? value })));
  createEffect(() => {
    const items = platforms();
    if (!items.some(item => item.value === platform())) setPlatform(items[0]?.value ?? null);
  });
  const [keys, { refetch: refetchKeys }] = createResource(platform, selected => window.Polymer.library.keys.list(selected));
  const matching = createMemo(() => {
    const query = filter().trim().toLowerCase();
    return (keys() ?? []).filter(key => !query || key.id.toLowerCase().includes(query) || key.titleId?.toLowerCase().includes(query));
  });
  const missing = () => shell.model.libraryGames.flatMap(game => game.platform === platform() && game.missingKey
    ? [{ label: game.name, value: game.missingKey.id }] : []);

  const dropChanged = (event: Event) => setDropActive((event as CustomEvent<boolean>).detail);
  const contentChanged = () => { void refetchPlatforms(); void refetchKeys(); };
  window.Polymer.scraping.addEventListener("dropactivechange", dropChanged);
  window.Polymer.library.addEventListener("contentchange", contentChanged);
  onCleanup(() => {
    window.Polymer.scraping.removeEventListener("dropactivechange", dropChanged);
    window.Polymer.library.removeEventListener("contentchange", contentChanged);
  });

  const remove = async () => {
    const key = removing(), selected = platform();
    if (!key || !selected) return;
    setPending(true);
    try {
      await window.Polymer.library.keys.remove(selected, key.namespace, key.id);
      setRemoving(null);
    } catch (error) {
      shell.setModel("error", error instanceof Error ? error.message : String(error));
    } finally {
      setPending(false);
    }
  };

  return <>
    <Section title="Import keys" description="Drop key files onto Polymer to import them. PlayStation 3 licenses (.rap) and Nintendo 3DS seed databases are supported.">
      <div class="horizon-key-drop" data-active={dropActive()}>
        <Icon name="key" class="horizon-key-drop-icon" strokeWidth={1.4} />
        <p>{dropActive() ? "Drop to import keys." : "Drag and drop key files here."}</p>
      </div>
    </Section>
    <Show when={platforms().length} fallback={<p class="hz-description">No keys have been imported.</p>}>
      <Section title="Platform">
        <Select label="Platform" items={platforms()} value={platform()} onValueChange={setPlatform} />
      </Section>
      <Show when={missing().length}>
        <Section title="Missing keys" description="These games need a key before they can start.">
          <DetailList items={missing()} />
        </Section>
      </Show>
      <Section title="Keys" description={`${keys()?.length ?? 0} ${keys()?.length === 1 ? "key" : "keys"} imported for this platform.`}>
        <Show when={(keys()?.length ?? 0) > VISIBLE_KEYS}>
          <TextField label="Filter" placeholder="Key or title ID" value={filter()} onValueChange={setFilter} />
        </Show>
        <Show when={matching().length} fallback={<p class="hz-description">{keys()?.length ? "No keys match the filter." : "No keys have been imported for this platform."}</p>}>
          <ul class="horizon-key-list">
            <For each={matching().slice(0, VISIBLE_KEYS)}>{key => <li>
              <ListRow label={key.id} description={[key.namespace === "content" ? "Content key" : "System key", key.titleId, `${key.byteLength} bytes`].filter(Boolean).join(" · ")}>
                <IconButton icon="trash" tone="danger" label={`Delete ${key.id}`} onClick={() => setRemoving(key)} />
              </ListRow>
            </li>}</For>
          </ul>
          <Show when={matching().length > VISIBLE_KEYS}>
            <p class="hz-description">Showing {VISIBLE_KEYS} of {matching().length} keys. Filter to narrow the list.</p>
          </Show>
        </Show>
      </Section>
    </Show>
    <ConfirmDialog open={!!removing()} title="Delete key?" pending={pending()} confirmLabel="Delete"
      description={`${removing()?.id ?? ""} will be removed. Games that need it will not start until it is imported again.`}
      onConfirm={() => void remove()} onClose={() => setRemoving(null)} />
  </>;
}
