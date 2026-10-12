import { Show, createResource, createSignal } from "solid-js";
import type { PlatformShader } from "@polymer-fe/frontend/shaders";
import { useShell } from "../../app/shell-context";
import { ActionRow, Button, Dialog, Icon, SelectableList, SettingRow, TextField, Toggle } from "../../ui";

const RESULT_LIMIT = 30;

export function ShaderPicker() {
  const shell = useShell();
  const [open, setOpen] = createSignal(false);
  const [query, setQuery] = createSignal("");
  const [selected, setSelected] = createSignal<string | null>(null);
  const [results] = createResource(() => query().trim(), text => text ? window.Polymer.shaders.search(text, { limit: RESULT_LIMIT }) : [], { initialValue: [] });
  const update = (pending: Promise<PlatformShader>) => pending.then(setting => shell.setModel("shader", setting), error => console.error("Failed to change shader", error));
  const current = () => shell.model.shader.preset?.split("/").pop()?.replace(/\.slangp$/i, "");
  const apply = (path: string) => {
    const entry = results().find(result => result.relativePath === path);
    if (entry) update(shell.shaders.apply(entry));
  };
  let row!: HTMLButtonElement;
  return <>
    <ActionRow ref={row} description={current() ? `${current()}${shell.model.shader.enabled ? "" : " (off)"}` : "No shader selected"} onClick={() => setOpen(true)}>
      <Icon name="theme" tone="action" /> Shader
    </ActionRow>
    <Dialog open={open()} onOpenChange={setOpen} title="Shader" class="horizon-shader-dialog" returnFocus={() => row} actions={<Button onClick={() => setOpen(false)}>Done</Button>}>
      <SettingRow description={current() ?? "No shader selected"}>
        <Toggle label="Shader" checked={shell.model.shader.enabled && !!shell.model.shader.preset} disabled={!shell.model.shader.preset} onCheckedChange={enabled => update(shell.shaders.setEnabled(enabled))} />
      </SettingRow>
      <SettingRow><TextField label="Find a shader" value={query()} placeholder="crt, lcd, bezel" onValueChange={setQuery} /></SettingRow>
      <Show when={results().length}>
        <SelectableList label="Shaders" value={selected()} items={results().map(entry => ({ value: entry.relativePath, label: entry.name, description: entry.category }))} onValueChange={setSelected} onActivate={apply} />
      </Show>
    </Dialog>
  </>;
}
