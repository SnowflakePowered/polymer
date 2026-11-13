import { For, Show, createSignal } from "solid-js";
import type { InputLayout } from "@polymer-fe/frontend";
import { SettingRow, TextField, useNavigation } from "../../ui";

export function LayoutPicker(props: { layouts: readonly InputLayout[]; value: string; onValueChange(id: string): void }) {
  const navigation = useNavigation();
  const [query, setQuery] = createSignal("");
  const leave = (event: KeyboardEvent) => {
    const direction = event.key === "ArrowUp" ? "up" : event.key === "ArrowDown" ? "down" : undefined;
    const root = (event.currentTarget as HTMLElement).closest<HTMLElement>(".horizon-mapping-overview");
    if (!direction || !root || !(event.target instanceof HTMLInputElement) || !navigation.move(root, direction)) return;
    event.preventDefault();
    navigation.focusChanged();
  };
  const consoleOf = (layout: InputLayout) => layout.consoles.join(", ");
  const controllerOf = (layout: InputLayout) => layout.name === layout.id ? "Layout" : layout.name;
  const matches = () => {
    const text = query().trim().toLowerCase();
    return props.layouts.filter(layout => !text || `${consoleOf(layout)} ${controllerOf(layout)} ${layout.id}`.toLowerCase().includes(text));
  };
  return <aside class="horizon-layout-picker" aria-label="Layout" onKeyDown={leave}>
    <SettingRow><TextField label="Find a layout" value={query()} placeholder="switch, playstation, ds" onValueChange={setQuery} /></SettingRow>
    <div class="horizon-layout-results" role="list" aria-label="Layouts">
      <Show when={matches().length} fallback={<p class="hz-description">No layouts match “{query().trim()}”.</p>}>
        <For each={matches()}>{layout =>
          <button type="button" role="listitem" class="horizon-layout-result" data-select-ring aria-current={layout.id === props.value || undefined} onClick={() => props.onValueChange(layout.id)}><span>{consoleOf(layout)}</span><span class="horizon-layout-controller">{controllerOf(layout)}</span></button>
        }</For>
      </Show>
    </div>
  </aside>;
}

