import { For, Show, createSignal } from "solid-js";
import type { InputLayout } from "@polymer-fe/frontend";
import { useNavigation } from "../../../ui";

export function LayoutPicker(props: { layouts: readonly InputLayout[]; value: string; onValueChange(id: string): void }) {
  const navigation = useNavigation();
  const [query, setQuery] = createSignal("");
  const consoleOf = (layout: InputLayout) => layout.consoles.join(", ");
  const controllerOf = (layout: InputLayout) => layout.name === layout.id ? "Layout" : layout.name;
  const matches = () => {
    const text = query().trim().toLowerCase();
    return props.layouts.filter(layout => !text || `${consoleOf(layout)} ${controllerOf(layout)} ${layout.id}`.toLowerCase().includes(text));
  };
  const leave = (event: KeyboardEvent) => {
    const direction = event.key === "ArrowUp" ? "up" : event.key === "ArrowDown" ? "down" : undefined;
    const root = (event.currentTarget as HTMLElement).closest<HTMLElement>(".vp-paged-content");
    if (!direction || !root || !(event.target instanceof HTMLInputElement) || !navigation.move(root, direction)) return;
    event.preventDefault();
    navigation.focusChanged();
  };
  return <aside class="vp-layout-picker" aria-label="Layout" onKeyDown={leave}>
    <input class="vp-text-input" type="text" aria-label="Find a layout" placeholder="Find a layout" value={query()} onInput={event => setQuery(event.currentTarget.value)} />
    <div class="vp-layout-results" role="list" aria-label="Layouts">
      <Show when={matches().length} fallback={<p class="vp-layout-empty">No layouts match “{query().trim()}”.</p>}>
        <For each={matches()}>{layout =>
          <button type="button" role="listitem" class="vp-layout-result" aria-current={layout.id === props.value || undefined} onClick={() => props.onValueChange(layout.id)}>
            <span>{consoleOf(layout)}</span><span class="vp-layout-controller">{controllerOf(layout)}</span>
          </button>
        }</For>
      </Show>
    </div>
  </aside>;
}
