import { Tabs } from "@ark-ui/solid";
import { For, Show } from "solid-js";
import type { EmulatorConfiguration, PackageConfiguration } from "../../../../../../js/snowglass";
import { ChoiceControl } from "../../components/FormControls";
import { friendly } from "../../app/launch";
import "./ControllerSection.css";

export function ControllerSection(props: {
  game: PackageConfiguration;
  emulator: EmulatorConfiguration;
  update(change: (emulator: EmulatorConfiguration) => void): void;
}) {
  return <section class="horizon-controller-section">
    <header><p>Input assignments</p><h2>Controllers</h2></header>
    <For each={props.emulator.groups}>{(group, groupIndex) => <article class="horizon-controller-group">
      <div class="horizon-controller-heading">
        <h3>{friendly(group.name)}</h3>
        <div class="horizon-connected"><ChoiceControl label="Connected" value={String(group.connected)} items={Array.from({ length: group.ports.length + 1 }, (_, value) => ({ value: String(value), label: String(value) }))} onChange={(value) => props.update((emulator) => { emulator.groups[groupIndex()].connected = Number(value); })} /></div>
      </div>
      <Show when={group.connected > 0} fallback={<p class="horizon-controller-empty">No controllers connected.</p>}>
        <Tabs.Root defaultValue={`port-${group.ports[0]?.index || 0}`}>
          <Tabs.List class="horizon-port-tabs"><Tabs.Indicator /><For each={group.ports.slice(0, group.connected)}>{(port) => <Tabs.Trigger value={`port-${port.index}`}>Port {port.index + 1}</Tabs.Trigger>}</For></Tabs.List>
          <For each={group.ports.slice(0, group.connected)}>{(port, portIndex) => <Tabs.Content value={`port-${port.index}`} class="horizon-port-content">
            <ChoiceControl label="Device" value={port.selectedDevice} items={["K", "0", "1", "2", "3"].map((value) => ({ value, label: value === "K" ? "Keyboard + mouse" : `Controller ${Number(value) + 1}` }))} onChange={(value) => props.update((emulator) => { emulator.groups[groupIndex()].ports[portIndex()].selectedDevice = value; })} />
            <ChoiceControl label="Layout" value={port.selectedLayout} items={port.layouts.map((value) => ({ value, label: friendly(value) }))} onChange={(value) => props.update((emulator) => { emulator.groups[groupIndex()].ports[portIndex()].selectedLayout = value; })} />
          </Tabs.Content>}</For>
        </Tabs.Root>
      </Show>
    </article>}</For>
  </section>;
}
