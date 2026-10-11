import { For, Show, createResource } from "solid-js";
import { Section, Select, SettingRow } from "../../ui";
import type { PlatformConfiguration } from "@polymer-fe/frontend";
import { useShell } from "../../app/shell-context";

export function PlatformSettings() {
  const shell = useShell();
  const [platforms, { mutate }] = createResource(() => window.Polymer.configuration.platforms.list());
  const update = (platform: PlatformConfiguration, emulator: string | null) => {
    void window.Polymer.configuration.platforms.update(platform.platform, { emulator })
      .then((updated) => mutate((entries) => entries?.map((entry) => entry.platform === updated.platform ? updated : entry)))
      .catch((error) => shell.setModel("error", error instanceof Error ? error.message : String(error)));
  };
  return <Section title="Platforms" description="Choose the emulator each platform uses. A game can still choose its own emulator in Game Settings.">
    <Show when={platforms()} fallback={<p role="status">Loading…</p>}>
      <For each={platforms()}>{(platform) => <SettingRow>
        <Select
          label={window.Polymer.machine(platform.platform)?.name ?? platform.platform}
          value={platform.settings.emulator ?? ""}
          items={[{ value: "", label: `Automatic (${platform.emulators[0]})` }, ...platform.emulators.map((emulator) => ({ value: emulator, label: emulator }))]}
          onValueChange={(value) => update(platform, value || null)}
        />
      </SettingRow>}</For>
    </Show>
  </Section>;
}
