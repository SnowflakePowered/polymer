import { Collapsible } from "@ark-ui/solid";
import { For } from "solid-js";
import type { EmulatorConfiguration, PackageConfiguration } from "../../../../../../js/polymer";
import { ChoiceControl, OptionControl } from "../../components/FormControls";
import { friendly } from "../../app/launch";
import "./SettingsSection.css";

export function SettingsSection(props: {
  game: PackageConfiguration;
  emulator: EmulatorConfiguration;
  selectEmulator(value: string): void;
  updateConfiguration(group: string, option: string, value: boolean | number | string | null): void;
}) {
  return <aside class="horizon-settings-section">
    <p class="horizon-settings-kicker">Game setup</p>
    <h2>Emulation</h2>
    <ChoiceControl label="Emulator" value={props.game.selectedEmulator} items={props.game.candidates.map((item) => ({ value: item.id, label: friendly(item.id) }))} onChange={props.selectEmulator} />
    <Collapsible.Root defaultOpen>
      <Collapsible.Trigger class="horizon-settings-trigger">Advanced settings <Collapsible.Indicator>⌄</Collapsible.Indicator></Collapsible.Trigger>
      <Collapsible.Content class="horizon-settings-list">
        <For each={props.emulator.configuration?.groups || []}>{(group) => <section>
          <h3>{group.title}</h3>
          <For each={group.options}>{(option) => <OptionControl
            option={option}
            value={props.emulator.userConfiguration?.[group.key]?.[option.key] ?? option.default}
            onChange={(value) => props.updateConfiguration(group.key, option.key, value)}
          />}</For>
        </section>}</For>
      </Collapsible.Content>
    </Collapsible.Root>
  </aside>;
}
