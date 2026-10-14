import { For, Match, Show, Switch, createResource } from "solid-js";
import type { Plugin, PluginOption } from "@polymer-fe/frontend";
import { Section, Select, SettingRow, Toggle } from "../../ui";
import { useShell } from "../../app/shell-context";

export function PluginSettings() {
  const shell = useShell();
  const [plugins, { refetch }] = createResource(async () => {
    const entries = await Promise.all(window.Polymer.plugin.list().map(async (plugin) => ({ plugin, options: await plugin.config() })));
    return entries.filter((entry) => entry.options.length);
  });
  const configure = (plugin: Plugin, option: PluginOption, value: unknown) => {
    void plugin.configure(option.key, value)
      .then(() => refetch())
      .catch((error) => shell.setModel("error", error instanceof Error ? error.message : String(error)));
  };
  return <Show when={plugins()} fallback={<p role="status">Loading…</p>}>
    <For each={plugins()} fallback={<Section title="Plugins" description="No plugin has settings." />}>{({ plugin, options }) =>
      <Section title={plugin.id} description={`${plugin.class} plugin ${plugin.version}`}>
        <For each={options}>{(option) => <SettingRow>
          <Switch fallback={<p>{option.display}: {String(option.value)}</p>}>
            <Match when={option.choices.length}>
              <Select
                label={option.display}
                value={String(option.value)}
                items={option.choices.map((choice) => ({ value: String(choice), label: String(choice) }))}
                onValueChange={(value) => configure(plugin, option, option.choices.find((choice) => String(choice) === value))}
              />
            </Match>
            <Match when={typeof option.default === "boolean"}>
              <Toggle label={option.display} checked={option.value === true} onCheckedChange={(checked) => configure(plugin, option, checked)} />
            </Match>
          </Switch>
        </SettingRow>}</For>
      </Section>}</For>
  </Show>;
}
