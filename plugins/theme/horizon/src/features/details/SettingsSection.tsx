import { For, Show, createMemo, createSignal } from "solid-js";
import type { EmulatorConfiguration, PackageConfiguration } from "@polymer-fe/frontend";
import { CategoryNavigation, Select, SettingRow } from "../../ui";
import { useInputScope, useNavigation } from "../../ui/input";
import { OptionControl } from "./OptionControl";
import { ControllerSection } from "./ControllerSection";
import "./SettingsSection.css";

export function SettingsSection(props: {
  game: PackageConfiguration;
  emulator: EmulatorConfiguration;
  selectEmulator(value: string | null): void;
  updateEmulator(change: (emulator: EmulatorConfiguration) => void): void;
  updateConfiguration(group: string, option: string, value: boolean | number | string | null): void;
}) {
  let categoryPane!: HTMLElement, panel!: HTMLElement;
  const navigation = useNavigation();
  useInputScope(() => ({ root: panel, navigation: true, handle: action => {
    if (action.type !== "move" || action.direction !== "left") return false;
    categoryPane.querySelector<HTMLElement>('[aria-current="true"]')?.focus();
    return true;
  } }));
  const [selected, setSelected] = createSignal("emulation");
  const categories = createMemo(() => [
    { key: "emulation", title: "Emulation" },
    { key: "controllers", title: "Controller topology" },
    ...(props.emulator.configuration?.groups || []).map((group) => ({ key: `configuration:${group.key}`, title: group.title })),
  ]);
  const platformDefault = () => props.game.candidates.find((item) => item.id === props.game.platformEmulator)?.id ?? props.game.candidates[0]?.id;
  const active = () => categories().find((category) => category.key === selected()) || categories()[0];
  const group = () => props.emulator.configuration?.groups.find((item) => `configuration:${item.key}` === active().key);

  return <div class="horizon-settings-section">
    <aside ref={categoryPane} class="horizon-settings-categories"><CategoryNavigation label="Game settings categories" value={active().key} items={categories().map(item => ({ value: item.key, label: item.title }))} onValueChange={setSelected} onEnter={() => navigation.items(panel)[0]?.focus()} /></aside>
    <section ref={panel} id="horizon-settings-panel" class="horizon-settings-panel" aria-labelledby="horizon-settings-title">
      <h2 id="horizon-settings-title">{active().title}</h2>
      <Show when={active().key === "emulation"}>
        <SettingRow>
          <Select label="Emulator" value={props.game.gameEmulator ?? ""} items={[{ value: "", label: `Platform default (${platformDefault()})` }, ...props.game.candidates.map((item) => ({ value: item.id, label: item.id }))]} onValueChange={value => props.selectEmulator(value || null)} />
        </SettingRow>
      </Show>
      <Show when={active().key === "controllers"}>
        <ControllerSection emulator={props.emulator} update={props.updateEmulator} />
      </Show>
      <Show when={group()?.description}><p class="horizon-settings-description">{group()?.description}</p></Show>
      <For each={group()?.options}>{(option) => <SettingRow>
        <OptionControl
          option={option}
          value={props.emulator.userConfiguration?.[group()?.key ?? ""]?.[option.key] ?? option.default}
          onChange={(value) => { const current = group(); if (current) props.updateConfiguration(current.key, option.key, value); }}
        />
      </SettingRow>}</For>
    </section>
  </div>;
}
