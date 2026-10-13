import { For, Show, createEffect, createResource, createSignal, onCleanup, onMount } from "solid-js";
import type { ShaderPresetEntry } from "@polymer-fe/frontend";
import type { PlatformShader } from "@polymer-fe/frontend/shaders";
import { useShell } from "../../app/shell";
import { Button, Modal, TextField, ToggleField, useNavigation } from "../../ui";

const RESULT_LIMIT = 20;
const VERTICAL = { ArrowUp: "up", ArrowDown: "down" } as const;

function Shaders(props: { onDone(): void }) {
  const shell = useShell();
  const navigation = useNavigation();
  const [query, setQuery] = createSignal("");
  const [results] = createResource(() => query().trim(), text => text ? window.Polymer.shaders.search(text, { limit: RESULT_LIMIT }) : [], { initialValue: [] });
  const update = (pending: Promise<PlatformShader>) => pending.then(setting => shell.update("shader", setting), shell.fail);
  const current = () => shell.state.shader.preset?.split("/").pop()?.replace(/\.slangp$/i, "");
  const apply = (entry: ShaderPresetEntry) => update(shell.shaders.apply(entry));
  onMount(() => {
    const keydown = (event: KeyboardEvent) => {
      const direction = VERTICAL[event.key as keyof typeof VERTICAL];
      const modal = event.target instanceof Element && event.target.closest(".vp-shader input") ? event.target.closest<HTMLElement>(".vp-shader") : null;
      if (!direction || !modal || !navigation.move(modal, direction)) return;
      event.preventDefault();
      navigation.focusChanged();
    };
    window.addEventListener("keydown", keydown);
    onCleanup(() => window.removeEventListener("keydown", keydown));
  });
  return <Modal open class="vp-shader" onClose={props.onDone} title="Shader" initialFocus={() => document.querySelector<HTMLElement>(".vp-shader input")}
    buttons={<Button primary onClick={props.onDone}>Done</Button>}>
    <ToggleField label="Shader" description={current() ?? "No shader selected"} checked={shell.state.shader.enabled && !!shell.state.shader.preset} disabled={!shell.state.shader.preset}
      onChange={enabled => update(shell.shaders.setEnabled(enabled))} />
    <TextField label="Find a shader" value={query()} placeholder="crt, lcd, bezel" bottomSeparator="none" onChange={setQuery} />
    <div class="vp-shader-results">
      <For each={results()}>{entry =>
        <Button class="vp-shader-result" data-active={shell.state.shader.preset === entry.relativePath || undefined} onClick={() => apply(entry)}>
          <span class="vp-shader-name">{entry.name}</span><small>{entry.category}</small>
        </Button>
      }</For>
    </div>
  </Modal>;
}

export function ShaderModal() {
  const shell = useShell();
  const open = () => shell.state.menu === "shader" && !!shell.state.launch;
  createEffect(() => {
    const launch = shell.state.launch;
    if (!launch || !open()) return;
    const release = shell.pauses.acquire(launch);
    onCleanup(() => void release().catch(error => console.error("Failed to resume emulator", error)));
  });
  return <Show when={open()}><Shaders onDone={() => shell.update("menu", "quick")} /></Show>;
}
