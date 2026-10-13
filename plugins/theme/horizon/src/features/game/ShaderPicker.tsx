import { For, Show, createResource, createSignal, onCleanup, onMount } from "solid-js";
import type { PlatformShader } from "@polymer-fe/frontend/shaders";
import { useShell } from "../../app/shell-context";
import { SettingRow, TextField, Toggle, useNavigation } from "../../ui";
import "./ShaderPicker.css";

const RESULT_LIMIT = 30;
const VERTICAL = { ArrowUp: "up", ArrowDown: "down" } as const;

function currentShader(preset: string | null) {
  return preset?.split("/").pop()?.replace(/\.slangp$/i, "");
}

export function ShaderPanel() {
  const shell = useShell();
  const [query, setQuery] = createSignal("");
  const search = () => query().trim();
  const [results] = createResource(search, text => window.Polymer.shaders.search(text, { limit: RESULT_LIMIT }), { initialValue: [] });
  const update = (pending: Promise<PlatformShader>) => pending.then(setting => shell.setModel("shader", setting), error => console.error("Failed to change shader", error));
  const current = () => currentShader(shell.model.shader.preset);
  const navigation = useNavigation();
  let panel!: HTMLDivElement;
  onMount(() => {
    const keydown = (event: KeyboardEvent) => {
      const direction = VERTICAL[event.key as keyof typeof VERTICAL];
      if (!direction || !(event.target instanceof Element && event.target.closest(".horizon-shader-panel input")) || !navigation.move(panel, direction)) return;
      event.preventDefault();
      navigation.focusChanged();
    };
    window.addEventListener("keydown", keydown);
    onCleanup(() => window.removeEventListener("keydown", keydown));
  });
  return <div ref={panel} class="horizon-shader-panel">
    <h2 class="horizon-shader-heading">Shader</h2>
    <SettingRow description={current() ?? "No shader selected"}>
      <Toggle label="Shader" checked={shell.model.shader.enabled && !!shell.model.shader.preset} disabled={!shell.model.shader.preset} onCheckedChange={enabled => update(shell.shaders.setEnabled(enabled))} />
    </SettingRow>
    <SettingRow><TextField label="Find a shader" value={query()} placeholder="crt, lcd, bezel" onValueChange={setQuery} /></SettingRow>
    <div class="horizon-shader-results" role="list" aria-label="Shaders" aria-busy={results.loading || undefined}>
      <Show when={search()} fallback={<p class="hz-description">Type to search the shader library.</p>}>
        <Show when={results().length || results.loading} fallback={<p class="hz-description">No shaders match “{search()}”.</p>}>
          <For each={search() ? results() : []}>{entry =>
            <button type="button" role="listitem" class="horizon-shader-result" data-select-ring aria-current={shell.model.shader.preset === entry.relativePath || undefined} onClick={() => update(shell.shaders.apply(entry))}>
              <span>{entry.name}</span><small>{entry.category}</small>
            </button>
          }</For>
        </Show>
      </Show>
    </div>
  </div>;
}
