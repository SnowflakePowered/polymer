import { For, Show } from "solid-js";
import type { EmulatorConfiguration } from "../../../../../../js/polymer";
import { Select, SettingRow } from "../../ui";
import "./ControllerSection.css";

export function ControllerSection(props: {
  emulator: EmulatorConfiguration;
  update(change: (emulator: EmulatorConfiguration) => void): void;
}) {
  const layouts = new Map(window.Polymer.controllers.layouts.map((layout) => [layout.id, String(layout.name)]));
  return <div class="horizon-controller-section">
    <Show when={props.emulator.groups.length} fallback={<p class="horizon-settings-description">This emulator has no controller ports.</p>}>
      <For each={props.emulator.groups}>{(group, groupIndex) => <section class="horizon-controller-group">
        <Show when={props.emulator.groups.length > 1}><h3>{group.name}</h3></Show>
        <For each={group.ports}>{(port, portIndex) => <SettingRow>
          <Select label={`Port ${port.index + 1}`} value={port.selectedLayout} items={port.layouts.map((value) => ({ value, label: layouts.get(value) ?? value }))} onValueChange={(value) => value !== null && props.update((emulator) => { emulator.groups[groupIndex()].ports[portIndex()].selectedLayout = value; })} />
        </SettingRow>}</For>
      </section>}</For>
    </Show>
    <p class="horizon-settings-description">Choose a controller layout for each port. Controllers connect automatically based on your controller order.</p>
  </div>;
}
