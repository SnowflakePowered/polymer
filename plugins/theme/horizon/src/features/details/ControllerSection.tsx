import { For, Show } from "solid-js";
import type { EmulatorConfiguration } from "@polymer-fe/frontend";
import { Select, SettingRow } from "../../ui";
import { useShell } from "../../app/shell-context";
import { PortMapping } from "./PortMapping";
import "./ControllerSection.css";

export function ControllerSection(props: {
  emulator: EmulatorConfiguration;
  update(change: (emulator: EmulatorConfiguration) => void): void;
}) {
  const shell = useShell();
  const layouts = new Map(window.Polymer.controllers.layouts.map((layout) => [layout.id, String(layout.name)]));
  return <div class="horizon-controller-section">
    <Show when={props.emulator.groups.length} fallback={<p class="horizon-settings-description">This emulator has no controller ports.</p>}>
      <For each={props.emulator.groups}>{(group, groupIndex) => <section class="horizon-controller-group">
        <Show when={props.emulator.groups.length > 1}><h3>{group.name}</h3></Show>
        <For each={group.ports}>{(port, portIndex) => <><SettingRow>
          <Select label={`Port ${port.index + 1}`} value={port.selectedLayout} items={port.layouts.map((value) => ({ value, label: layouts.get(value) ?? value }))} onValueChange={(value) => value !== null && props.update((emulator) => { emulator.groups[groupIndex()].ports[portIndex()].selectedLayout = value; })} />
        </SettingRow><Show when={shell.model.launchableId}>{launchable => <PortMapping launchable={launchable()} group={group.name} port={port.index} slot={props.emulator.groups.slice(0, groupIndex()).reduce((count, earlier) => count + earlier.ports.length, 0) + port.index} layoutId={port.selectedLayout} />}</Show></>}</For>
      </section>}</For>
    </Show>
    <p class="horizon-settings-description">Choose a controller layout for each port. Controllers connect automatically based on your controller order.</p>
  </div>;
}
