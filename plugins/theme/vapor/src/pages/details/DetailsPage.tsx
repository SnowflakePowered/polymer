import { For, Match, Show, Switch, createMemo, createResource, createSignal } from "solid-js";
import type { ConfigurationLayoutOption } from "@polymer-fe/frontend";
import { useShell } from "../../app/shell";
import { DropdownField, Field, Hero, NumberField, Page, PlayBar, PlayButton, SliderField, Tabs, TextField, ToggleField } from "../../ui";
import { platformName } from "../library/platform";
import "./details.css";

type Value = boolean | number | string | null;

function OptionField(props: { option: ConfigurationLayoutOption; value: Value; onChange(value: Value): void }) {
  const ranged = () => props.option.min !== undefined && props.option.max !== undefined && props.option.max - props.option.min <= 100;
  return <Switch fallback={<TextField label={props.option.title} description={props.option.description} value={String(props.value ?? "")} onChange={props.onChange} />}>
    <Match when={props.option.type === "bool"}><ToggleField label={props.option.title} description={props.option.description} checked={Boolean(props.value)} onChange={props.onChange} /></Match>
    <Match when={props.option.type === "select"}><DropdownField label={props.option.title} description={props.option.description} value={props.value === null ? null : String(props.value)} options={(props.option.choices ?? []).map(choice => ({ value: choice.key, label: choice.label }))} onChange={props.onChange} /></Match>
    <Match when={props.option.type === "int" && ranged()}><SliderField label={props.option.title} description={props.option.description} value={Number(props.value ?? 0)} min={props.option.min} max={props.option.max} onChange={props.onChange}
      format={value => props.option.choices?.find(choice => choice.key === String(value))?.label ?? String(value)}
      notches={props.option.max! - props.option.min! <= 10 ? Array.from({ length: props.option.max! - props.option.min! + 1 }, (_, index) => props.option.choices?.length ? `${props.option.min! + index}×` : String(props.option.min! + index)) : undefined} /></Match>
    <Match when={props.option.type === "int" || props.option.type === "float"}><NumberField label={props.option.title} description={props.option.description} value={Number(props.value ?? 0)} min={props.option.min} max={props.option.max} step={props.option.type === "int" ? 1 : .1} onChange={props.onChange} /></Match>
  </Switch>;
}

export function DetailsPage() {
  const shell = useShell();
  const [tab, setTab] = createSignal("emulation");
  const game = () => shell.state.draft;
  const emulator = () => shell.emulator();
  const entry = createMemo(() => shell.state.games.find(item => item.path === game()?.path) ?? shell.state.recent.find(item => item.path === game()?.path));
  const [showcase] = createResource(() => game()?.path, path => shell.library.showcase(path));
  const [banner] = createResource(() => game()?.path, async path => {
    const [, storage, title] = /^artifact:\/\/[^/]+\/([^/]+)\/(.+)$/.exec(path) ?? [];
    if (!title) return undefined;
    const volume = (await window.Polymer.consolestorage.Storage.list({ kind: "nand" })).find(item => item.id === storage);
    const artifact = (await volume?.artifacts.list())?.find(item => item.id === title);
    return artifact?.icons.find(icon => /^data:application\/x-(3ds|wii)-banner/.test(icon));
  });
  const layouts = new Map(window.Polymer.controllers.layouts.map(layout => [String(layout.id), String(layout.name)]));
  const tabs = createMemo(() => [
    { id: "emulation", title: "Emulation" },
    { id: "controllers", title: "Controllers" },
    ...(emulator()?.configuration?.groups ?? []).map(group => ({ id: `group:${group.key}`, title: group.title })),
  ]);
  const group = () => emulator()?.configuration?.groups.find(item => `group:${item.key}` === tab());
  const platformDefault = () => game()?.candidates.find(item => item.id === game()?.platformEmulator)?.id ?? game()?.candidates[0]?.id ?? "";
  const lastPlayed = () => {
    const played = shell.state.recent.find(item => item.path === game()?.path)?.playedAt;
    return played ? new Date(played).toLocaleDateString() : "Never";
  };
  return <Show when={game()} keyed>{draft => <Page class="vp-details" hints={[{ action: "activate", label: "Select" }, { action: "back", label: "Back" }]} onAction={action => {
    if (action.type !== "back") return false;
    shell.closeDetails();
    return true;
  }}>
    <Hero title={entry()?.name ?? draft.name} art={showcase()?.background} logo={showcase()?.logo} banner={banner()}>
      <PlayBar action={<PlayButton data-autofocus state={shell.state.phase.kind === "idle" ? "play" : "launching"} onClick={() => void shell.launch()} />}
        stats={[{ label: "Platform", value: platformName(draft.platform) }, { label: "Last played", value: lastPlayed() }, { label: "Emulator", value: emulator()?.id ?? "None" }]} />
    </Hero>
    <section class="vp-details-body">
      <Tabs tabs={tabs()} active={tab()} onChange={setTab} />
      <div class="vp-details-panel">
        <Show when={tab() === "emulation"}>
          <DropdownField label="Emulator" description="The emulator used to play this game." value={draft.gameEmulator ?? ""}
            options={[{ value: "", label: `Platform default (${platformDefault()})` }, ...draft.candidates.map(item => ({ value: item.id, label: item.id }))]}
            onChange={value => shell.selectEmulator(value || null)} />
          <Field label="Location" description={decodeURI(draft.path.replace(/^file:\/\/\//, ""))} />
        </Show>
        <Show when={tab() === "controllers"}>
          <Show when={emulator()?.groups.length} fallback={<Field label="No controller ports" description="This emulator does not take controller input." />}>
            <For each={emulator()!.groups}>{(group, groupIndex) => <For each={group.ports}>{(port, portIndex) =>
              <DropdownField label={`${emulator()!.groups.length > 1 ? `${group.name} · ` : ""}Port ${port.index + 1}`} value={port.selectedLayout}
                options={port.layouts.map(value => ({ value, label: layouts.get(value) ?? value }))} onChange={value => shell.selectLayout(groupIndex(), portIndex(), value)} />
            }</For>}</For>
          </Show>
        </Show>
        <Show when={group()} keyed>{current => <>
          <Show when={current.description}><p class="vp-details-description">{current.description}</p></Show>
          <For each={current.options}>{option => <OptionField option={option} value={emulator()?.userConfiguration?.[current.key]?.[option.key] ?? option.default} onChange={value => shell.configure(current.key, option.key, value)} />}</For>
        </>}</Show>
      </div>
    </section>
  </Page>}</Show>;
}
