import { Collapsible } from "@ark-ui/solid";
import { For } from "solid-js";
import type { EmulatorConfiguration, PackageConfiguration } from "../../../../../../js/snowglass";
import { ChoiceControl, OptionControl } from "../../components/FormControls";
import { activeOptions, friendly } from "../../app/launch";
import "./SettingsSection.css";

export function SettingsSection(props: {
  game: PackageConfiguration;
  emulator: EmulatorConfiguration;
  selectEmulator(value: string): void;
  update(change: (emulator: EmulatorConfiguration) => void): void;
}) {
  return <aside class="horizon-settings-section">
    <p class="horizon-settings-kicker">Game setup</p>
    <h2>Emulation</h2>
    <ChoiceControl label="Emulator" value={props.game.selectedEmulator} items={props.game.candidates.map((item) => ({ value: item.id, label: friendly(item.id) }))} onChange={props.selectEmulator} />
    <Collapsible.Root defaultOpen>
      <Collapsible.Trigger class="horizon-settings-trigger">Advanced settings <Collapsible.Indicator>⌄</Collapsible.Indicator></Collapsible.Trigger>
      <Collapsible.Content class="horizon-settings-list">
        <For each={activeOptions(props.emulator, props.game.platform)}>{(option) => <OptionControl option={option} onChange={(value) => props.update((emulator) => { const target = emulator.options.find((item) => item.path === option.path); if (target) target.value = value; })} />}</For>
      </Collapsible.Content>
    </Collapsible.Root>
  </aside>;
}
