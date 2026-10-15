import { For, Show, createMemo, createResource, createSignal, onMount } from "solid-js";
import type { ConsoleStorageVolume, StorageSave } from "@polymer-fe/frontend";
import { css } from "../../../styled-system/css";
import { Screen, ScreenHeader, MasterDetail, CategoryNavigation, Section, TileGrid, Tile, Icon, ActionDialog, ConfirmDialog, EmptyState, ErrorState, Spinner, type MenuAction } from "../../ui";
import { AppSidebar } from "../../components/AppSidebar";
import { useShell } from "../../app/shell-context";
import { ProfileNameDialog, SaveDetailsDialog, SaveIcon, type NameRequest } from "./SaveDialogs";

type SaveEntry = { key: string; volume: ConsoleStorageVolume; save: StorageSave };
type Menu = "profile" | "save" | "fork" | "deleteSave" | "deleteProfile";

const shellLayout = css({ display: "flex", height: "100%", color: "text", bg: "surface" });
const screen = css({ minWidth: "0", flex: "1" });
const profileRow = css({ display: "flex", alignItems: "center", gap: "1rem", padding: "calc(var(--hz-focus-width) + 6px)", marginBottom: "1rem", "& > p": { margin: "0 0 0 .5rem", color: "muted", fontSize: "1.1rem" } });
const profileButton = css({ display: "flex", width: "3.5rem", height: "3.5rem", flex: "none", padding: "0", border: "0", borderRadius: "50%", bg: "transparent", cursor: "pointer", "--hz-ring-inset": "-6px", "--hz-ring-fill": "token(colors.surface)", "& > span": { display: "flex", width: "100%", height: "100%", alignItems: "center", justifyContent: "center", boxSizing: "border-box", border: "3px solid white", borderRadius: "50%", color: "white", fontSize: "1.5rem", fontWeight: "500", lineHeight: "1", paddingBottom: ".18em", paddingLeft: ".06em" }, "&[aria-pressed=false] > span": { color: "rgb(255 255 255 / 55%)", backgroundImage: "linear-gradient(rgb(0 0 0 / 50%), rgb(0 0 0 / 50%))" } });
const profileColors = ["#e60012", "#1a6fd8", "#1e9e5a", "#e08a00", "#8e44ad", "#12a3a3", "#d63384", "#6d4c41"];
const saveGrid = css({ gridTemplateColumns: "repeat(auto-fill, 4.25rem)", gap: ".5rem" });
const saveTile = css({ display: "flex", width: "4.25rem", height: "4.25rem", padding: "var(--hz-focus-width)", bg: "transparent", borderRadius: "card", "&[data-selected]": { boxShadow: "none" } });
const saveIcon = css({ width: "100%", height: "100%", flex: 1, minHeight: 0, borderRadius: "control", objectFit: "contain", display: "flex", alignItems: "center", justifyContent: "center", bg: "overlay", "& svg": { width: "1.5rem", height: "1.5rem" } });

export function SavesPage() {
  const shell = useShell();
  const api = window.Polymer.consolestorage;
  const [volumes, { refetch: reload }] = createResource(async () => (await api.Storage.list()).filter(volume => volume.kind !== "nand"));
  const [platform, setPlatform] = createSignal<string | null>(shell.model.draft?.platform ?? null);
  const [profileId, setProfileId] = createSignal<string | null>(null);
  const [selectedId, setSelectedId] = createSignal<string | null>(null);
  const [menu, setMenu] = createSignal<Menu | null>(null);
  const [target, setTarget] = createSignal<{ profile?: ConsoleStorageVolume; entry?: SaveEntry }>({});
  const [naming, setNaming] = createSignal<NameRequest | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");

  const platforms = createMemo(() => [...new Set((volumes() ?? []).map(volume => volume.platform))].map(value => ({ value, label: window.Polymer.machine(value)?.name ?? value })));
  const activePlatform = () => platforms().find(item => item.value === platform())?.value ?? platforms()[0]?.value ?? null;
  const profiles = () => (volumes() ?? []).filter(volume => volume.platform === activePlatform() && volume.kind !== "battery");
  const batteries = () => (volumes() ?? []).filter(volume => volume.platform === activePlatform() && volume.kind === "battery");
  const profile = () => profiles().find(volume => volume.id === profileId()) ?? profiles()[0];
  const profileName = (volume?: ConsoleStorageVolume) => volume?.label || "Default";
  const [saves, { refetch: relist }] = createResource<SaveEntry[], ConsoleStorageVolume | ConsoleStorageVolume[]>(() => profile() ?? batteries(), async source => {
    const sources = Array.isArray(source) ? source : [source];
    return (await Promise.all(sources.map(async volume => (await volume.saves.list()).map(save => ({ key: `${volume.id}:${save.id}`, volume, save: { ...save, title: save.title.normalize("NFKC").trim() } }))))).flat();
  });
  const selected = () => saves()?.find(entry => entry.key === selectedId()) ?? saves()?.[0];

  const run = async (action: () => Promise<unknown>) => {
    if (busy()) return;
    setBusy(true); setError("");
    try { await action(); }
    catch (reason) { setError(reason instanceof Error ? reason.message : String(reason)); }
    finally { setBusy(false); }
  };
  let page!: HTMLDivElement;
  const focusTarget = () => page.querySelector<HTMLElement>(`[data-save-key="${CSS.escape(selectedId() ?? "")}"]`) ?? page.querySelector<HTMLElement>("[aria-label=Profiles] [aria-pressed=true]") ?? page.querySelector<HTMLElement>(".hz-tile-grid .hz-tile");
  const refresh = async (nextProfile?: string) => {
    await reload();
    if (nextProfile) { setProfileId(nextProfile); setSelectedId(null); }
    await relist();
    queueMicrotask(() => { const active = document.activeElement; if (!active || active === document.body || !active.isConnected) focusTarget()?.focus(); });
  };
  const openMenu = (kind: "profile" | "save", entry?: SaveEntry) => { setTarget({ profile: entry?.volume.kind === "battery" ? undefined : profile(), entry }); setMenu(kind); };
  const fork = (entry: SaveEntry, target: ConsoleStorageVolume) => run(async () => {
    await entry.volume.saves.copyTo(entry.save.id, target);
    await refresh(target.id);
  });
  const newProfile = (then?: (volume: ConsoleStorageVolume) => Promise<unknown>) => setNaming({
    title: "New profile", label: "", confirm: "Create",
    submit: label => run(async () => {
      const volume = await api.Storage.create(target().profile!.kind as "saveRepo" | "memoryCard", target().profile!.platform, { label });
      await then?.(volume);
      await refresh(volume.id);
    }),
  });

  const profileActions = (volume: ConsoleStorageVolume): MenuAction[] => [
    { label: "+ New profile", run: () => newProfile() },
    { label: `Rename ${profileName(volume)}…`, run: () => setNaming({ title: "Rename profile", label: profileName(volume), confirm: "Rename", submit: label => run(async () => { await volume.setLabel(label); await refresh(volume.id); }) }) },
    { label: `Duplicate ${profileName(volume)}…`, run: () => setNaming({ title: "Duplicate profile", label: `${profileName(volume)} copy`, confirm: "Duplicate", submit: label => run(async () => refresh((await volume.fork(label)).id)) }) },
    { label: `Delete ${profileName(volume)}…`, danger: true, disabled: profiles().length < 2, run: () => queueMicrotask(() => setMenu("deleteProfile")) },
  ];
  const forkActions = (entry: SaveEntry): MenuAction[] => [
    ...profiles().filter(volume => volume.id !== entry.volume.id).map(volume => ({ label: profileName(volume), run: () => void fork(entry, volume) })),
    { label: "+ New profile", run: () => queueMicrotask(() => newProfile(volume => entry.volume.saves.copyTo(entry.save.id, volume))) },
  ];

  onMount(() => { void reload(); });
  const platformName = () => platforms().find(item => item.value === activePlatform())?.label ?? "";
  const selectProfile = (value: string) => { if (value === profile()?.id) return; setProfileId(value); setSelectedId(null); };
  const changePlatform = (value: string) => { if (value === activePlatform()) return; setPlatform(value); setProfileId(null); setSelectedId(null); };
  const hints = () => [{ action: "move" as const, label: "Change pane" }, ...profiles().length > 1 ? [{ action: "previous" as const, label: "Change profile" }] : [], ...profile() ? [{ action: "details" as const, label: "Profile options" }] : [], { action: "back" as const, label: "Back" }, { action: "activate" as const, label: "Open" }];
  return <div ref={page} class={shellLayout}><AppSidebar /><Screen class={screen} hints={hints()} onAction={action => {
    if (action.type === "back") { shell.navigate("/"); return true; }
    if (action.type === "details" && profile()) { openMenu("profile"); return true; }
    if ((action.type === "previous" || action.type === "next") && profiles().length > 1) {
      const index = profiles().indexOf(profile()!) + (action.type === "previous" ? -1 : 1);
      selectProfile(profiles()[(index + profiles().length) % profiles().length].id);
      return true;
    }
    return false;
  }}>
    <ScreenHeader title="Save Data" />
    <Show when={error()}><ErrorState message={error()} /></Show>
    <Show when={!volumes.loading || volumes()} fallback={<Spinner label="Loading save data" />}><Show when={platforms().length} fallback={<EmptyState title="No save data" description="Saves appear here after you play a game." />}>
    <MasterDetail navigation={<CategoryNavigation label="Systems" value={activePlatform() ?? ""} items={platforms()} onValueChange={changePlatform} />}>
      <Show when={profiles().length}><div class={profileRow} role="group" aria-label="Profiles"><For each={profiles()}>{(volume, index) => <button type="button" class={profileButton} data-select-ring aria-pressed={volume === profile()} aria-label={profileName(volume)} title={profileName(volume)} onFocus={() => selectProfile(volume.id)} onClick={() => selectProfile(volume.id)}>
        <span style={{ "background-color": profileColors[index() % profileColors.length] }}>{profileName(volume).trim().charAt(0).toUpperCase()}</span>
      </button>}</For><p>{profileName(profile())}</p></div></Show>
      <Section title="Saves"><Show when={!saves.error} fallback={<ErrorState message={String(saves.error)} />}><Show when={saves()?.length} fallback={<Show when={saves()} fallback={<Spinner label="Loading saves" />}><EmptyState title="No saves" description={profile() ? `${profileName(profile())} has no saves yet.` : "Saves appear here after you play a game."} /></Show>}>
        <TileGrid class={saveGrid} aria-label="Saves"><For each={saves()}>{entry => <Tile class={saveTile} selected={selected() === entry} aria-label={entry.save.title} data-save-key={entry.key} onFocus={() => setSelectedId(entry.key)} onClick={() => { setSelectedId(entry.key); openMenu("save", entry); }}>
          <SaveIcon save={entry.save} class={saveIcon} />
        </Tile>}</For></TileGrid>
      </Show></Show></Section>
    </MasterDetail></Show></Show>
    <Show when={target().profile}>{volume => <>
      <ActionDialog open={menu() === "profile"} title={`Profiles · ${platformName()}`} actions={profileActions(volume())} returnFocus={focusTarget} onClose={() => setMenu(current => current === "profile" ? null : current)} />
      <ConfirmDialog open={menu() === "deleteProfile"} pending={busy()} title={`Delete ${profileName(volume())}?`} description="All saves and checkpoints in this profile are permanently removed." confirmLabel="Delete profile" returnFocus={focusTarget} onClose={() => setMenu(null)} onConfirm={() => void run(async () => { await volume().delete(); setProfileId(null); await refresh(); setMenu(null); })} />
    </>}</Show>
    <Show when={target().entry}>{entry => <>
      <SaveDetailsDialog open={menu() === "save"} entry={entry()} subtitle={[entry().volume.kind === "battery" ? null : profileName(entry().volume), platformName()].filter(Boolean).join(" · ")} pending={busy()} canFork={!!target().profile} returnFocus={focusTarget}
        onFork={() => setMenu("fork")} onDelete={() => setMenu("deleteSave")} onRestore={commit => run(async () => { await entry().volume.saves.restore(entry().save.id, commit); await relist(); })} onClose={() => setMenu(current => current === "save" ? null : current)} />
      <ActionDialog open={menu() === "fork"} title={`Copy ${entry().save.title} to…`} description={`Keeps the original in ${profileName(entry().volume)}. Switches to the profile you choose.`} actions={forkActions(entry())} returnFocus={focusTarget} onClose={() => setMenu(current => current === "fork" ? null : current)} />
      <ConfirmDialog open={menu() === "deleteSave"} pending={busy()} title={`Delete ${entry().save.title}?`} description="The save is removed from this profile. Its checkpoints are kept." confirmLabel="Delete save" returnFocus={focusTarget} onClose={() => setMenu(null)} onConfirm={() => void run(async () => { await entry().volume.saves.delete(entry().save.id); setSelectedId(null); await refresh(); setMenu(null); })} />
    </>}</Show>
    <ProfileNameDialog request={naming()} pending={busy()} returnFocus={focusTarget} onClose={() => setNaming(null)} />
  </Screen></div>;
}
