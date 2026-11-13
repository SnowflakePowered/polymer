import { For, Match, Show, Switch, createResource, createSignal, onCleanup, onMount } from "solid-js";
import type { LibrarySource, ManagedControllerDevice, PlatformConfiguration, Plugin, PluginOption } from "@polymer-fe/frontend";
import { useShell } from "../../app/shell";
import { Button, ConfirmModal, DropdownField, Field, Page, PagedSettings, ProgressBar, Throbber, ToggleField } from "../../ui";
import { platformName } from "../library/platform";
import "./settings.css";

function Platforms() {
  const shell = useShell();
  const [platforms, { mutate }] = createResource(() => window.Polymer.configuration.platforms.list());
  const choose = (platform: PlatformConfiguration, emulator: string) => void window.Polymer.configuration.platforms.update(platform.platform, { emulator: emulator || null })
    .then(updated => mutate(list => list?.map(item => item.platform === updated.platform ? updated : item)), shell.fail);
  return <Show when={!platforms.loading} fallback={<Throbber />}>
    <For each={platforms()}>{platform => <DropdownField label={platformName(platform.platform)} value={platform.settings.emulator ?? ""}
      options={[{ value: "", label: `Automatic (${platform.emulators[0] ?? "none"})` }, ...platform.emulators.map(id => ({ value: id, label: id }))]} onChange={value => choose(platform, value)} />}</For>
  </Show>;
}

function Controllers() {
  const [devices, setDevices] = createSignal<readonly ManagedControllerDevice[]>(window.Polymer.controllers.devices);
  onMount(() => {
    const changed = () => setDevices([...window.Polymer.controllers.devices]);
    for (const event of ["statechange", "playerchange"]) window.Polymer.controllers.addEventListener(event, changed);
    onCleanup(() => { for (const event of ["statechange", "playerchange"]) window.Polymer.controllers.removeEventListener(event, changed); });
  });
  const shell = useShell();
  return <><Field label="Controller order" description="Connect controllers and choose which player each one is."><Button onClick={() => shell.update("menu", "order")}>Change order</Button></Field>
  <Field label="Button mapping" description="Rebind buttons, sticks and keys for each controller."><Button onClick={() => shell.navigate("/mapping")}>Edit mapping</Button></Field>
  <h3 class="vp-section-title">Connected</h3>
  <Show when={devices().length} fallback={<Field label="No controllers" description="Connect a controller to see it here." />}>
    <For each={devices()}>{device => <Field label={device.name} description={device.connected ? device.index === null ? "Connected" : `Player ${device.index + 1}` : "Not connected"}>
      <Show when={device.battery?.level !== null && device.battery}>{battery => <div class="vp-settings-battery"><ProgressBar value={(battery().level ?? 0) / 100} /></div>}</Show>
    </Field>}</For>
  </Show></>;
}

const SOURCE_STATES: Record<LibrarySource["state"], string> = {
  watching: "Watching for changes",
  scanning: "Scanning…",
  offline: "Not reachable. Its games stay in the library until it returns.",
  idle: "Waiting",
};

function Sources() {
  const shell = useShell();
  const sources = window.Polymer.library.sources;
  const [list, setList] = createSignal<readonly LibrarySource[]>([]);
  const [removing, setRemoving] = createSignal<LibrarySource | null>(null);
  onMount(() => {
    void sources.list().then(setList, shell.fail);
    const changed = (event: Event) => setList((event as CustomEvent<readonly LibrarySource[]>).detail);
    sources.addEventListener("change", changed);
    onCleanup(() => sources.removeEventListener("change", changed));
  });
  const add = () => void showDirectoryPicker()
    .then(folder => sources.add(folder))
    .then(() => sources.list())
    .then(setList, error => { if (!(error instanceof DOMException && error.name === "AbortError")) shell.fail(error); });
  const remove = (source: LibrarySource) => void sources.remove(source.id)
    .then(() => sources.list())
    .then(setList, shell.fail);
  return <>
    <Field label="Add folder" description="Games in library folders are added automatically and kept up to date as files are added or removed. Installed PC games and installable packages are skipped.">
      <Button primary onClick={add}>Browse…</Button>
    </Field>
    <h3 class="vp-section-title">Library folders</h3>
    <Show when={list().length} fallback={<Field label="No folders" description="Add a folder to keep its games in your library." />}>
      <For each={list()}>{source => <Field label={source.root} description={SOURCE_STATES[source.state]}>
        <Button disabled={source.state !== "watching"} onClick={() => void sources.scan(source.id).catch(shell.fail)}>Rescan</Button>
        <Button danger onClick={() => setRemoving(source)}>Remove</Button>
      </Field>}</For>
    </Show>
    <ConfirmModal open={!!removing()} title="Remove folder?" confirm="Remove" danger
      description={`Games added from ${removing()?.root ?? ""} will be removed from the library. Games you imported yourself are kept, and no files are deleted.`}
      onConfirm={() => { const source = removing(); setRemoving(null); if (source) remove(source); }} onClose={() => setRemoving(null)} />
  </>;
}

function About() {
  const shell = useShell();
  const [plugins, { refetch }] = createResource(() => Promise.all(window.Polymer.plugin.list().map(async plugin => ({ plugin, options: await plugin.config() }))));
  const configure = (plugin: Plugin, option: PluginOption, value: unknown) => void plugin.configure(option.key, value)
    .then(() => refetch(), shell.fail);
  return <Show when={plugins()} fallback={<Throbber />}><For each={plugins()}>{({ plugin, options }) => <>
    <h3 class="vp-section-title">{plugin.id}</h3>
    <Field label="Version" description={`${plugin.class} plugin`}><span class="vp-settings-version">{plugin.version}</span></Field>
    <For each={options}>{option => <Switch fallback={<Field label={option.display} description={String(option.value)} />}>
      <Match when={option.choices.length}>
        <DropdownField label={option.display} value={String(option.value)} options={option.choices.map(choice => ({ value: String(choice), label: String(choice) }))}
          onChange={value => configure(plugin, option, option.choices.find(choice => String(choice) === value))} />
      </Match>
      <Match when={typeof option.default === "boolean"}>
        <ToggleField label={option.display} checked={option.value === true} onChange={checked => configure(plugin, option, checked)} />
      </Match>
    </Switch>}</For>
  </>}</For></Show>;
}

export function SettingsPage() {
  const shell = useShell();
  const [page, setPage] = createSignal("platforms");
  return <Page class="vp-settings" hints={[{ action: "activate", label: "Select" }, { action: "back", label: "Back" }]} onAction={action => {
    if (action.type !== "back") return false;
    shell.navigate("/");
    return true;
  }}>
    <PagedSettings title="Settings" active={page()} onChange={setPage} pages={[
      { id: "platforms", title: "Platforms", icon: "display", content: () => <Platforms /> },
      { id: "controllers", title: "Controllers", icon: "controller", content: () => <Controllers /> },
      { id: "sources", title: "Library folders", icon: "library", content: () => <Sources /> },
      { id: "about", title: "Plugins", icon: "info", separatorBefore: true, content: () => <About /> },
    ]} />
  </Page>;
}
