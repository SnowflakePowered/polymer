import { For, Show, createEffect, createMemo, createResource, createSignal, on } from "solid-js";
import type { ConsoleStorageVolume, SaveCommitOperation, StorageSave } from "../../../../../../js/polymer";
import { useShell } from "../../app/shell";
import { Button, ConfirmModal, ContextMenu, EmptyState, Field, Icon, Modal, Page, PagedSettings, Tabs, TextField, Throbber, type MenuAction } from "../../ui";
import { platformName } from "../library/platform";

type Entry = { key: string; volume: ConsoleStorageVolume; save: StorageSave };
type Naming = { title: string; label: string; confirm: string; submit(label: string): Promise<unknown> };

const OPERATIONS: Record<SaveCommitOperation, string> = { initialImport: "Imported", autosave: "Autosave", manual: "Manual", restore: "Restored" };
const size = (bytes: number) => `${Math.max(1, Math.ceil(bytes / 1024)).toLocaleString()} KB`;
const when = (timestamp: string) => {
  const date = new Date(timestamp), days = Math.round((new Date().setHours(0, 0, 0, 0) - new Date(timestamp).setHours(0, 0, 0, 0)) / 86400000);
  const time = date.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  return days === 0 ? `Today, ${time}` : days === 1 ? `Yesterday, ${time}` : `${date.toLocaleDateString()}, ${time}`;
};
const profileName = (volume?: ConsoleStorageVolume) => volume?.label || "Default";

function SaveIcon(props: { save: StorageSave }) {
  return <Show when={props.save.icon} fallback={<Icon name="disc" color="#67707b" />}>{icon => <img src={icon()} alt="" />}</Show>;
}

function NameModal(props: { request: Naming | null; busy: boolean; onClose(): void }) {
  const [label, setLabel] = createSignal("");
  createEffect(() => setLabel(props.request?.label ?? ""));
  const submit = async () => { if (label().trim()) { await props.request!.submit(label().trim()); props.onClose(); } };
  return <Modal open={!!props.request} onClose={props.onClose} title={props.request?.title ?? ""}
    buttons={<><Button primary disabled={props.busy || !label().trim()} onClick={() => void submit()}>{props.request?.confirm}</Button><Button disabled={props.busy} onClick={props.onClose}>Cancel</Button></>}>
    <form onSubmit={event => { event.preventDefault(); void submit(); }}><TextField label="Profile name" value={label()} onChange={setLabel} /></form>
  </Modal>;
}

function SaveModal(props: { entry: Entry | null; subtitle: string; busy: boolean; canCopy: boolean; onCopy(): void; onDelete(): void; onRestore(commit: string): Promise<unknown>; onClose(): void }) {
  const [history, { refetch }] = createResource(() => props.entry ?? false, async entry => (await entry.volume.saves.history(entry.save.id)).sort((a, b) => Date.parse(b.timestamp) - Date.parse(a.timestamp)));
  const [chosenId, setChosenId] = createSignal<string | null>(null);
  const [confirming, setConfirming] = createSignal(false);
  createEffect(on(() => props.entry, () => setChosenId(null)));
  const chosen = () => history()?.find(commit => commit.id === chosenId()) ?? history()?.[0];
  const restore = async () => {
    const commit = chosen();
    if (!commit) return;
    await props.onRestore(commit.id);
    await refetch();
    setChosenId(commit.id);
  };
  return <Show when={props.entry}>{entry => <>
    <Modal open onClose={props.onClose} title={entry().save.title} description={props.subtitle}
      buttons={<>
        <Show when={props.canCopy}><Button disabled={props.busy} onClick={props.onCopy}>Copy to profile</Button></Show>
        <Button primary disabled={props.busy || !chosen() || chosen() === history()?.[0]} onClick={() => setConfirming(true)}>Restore checkpoint</Button>
        <Button danger disabled={props.busy} onClick={props.onDelete}>Delete</Button>
      </>}>
      <div class="vp-save-overview"><div class="vp-field-icon"><SaveIcon save={entry().save} /></div>
        <dl class="vp-detail-grid">
          <dt>Size</dt><dd>{size(entry().save.byteLength)}{entry().save.blocks === null ? "" : ` · ${entry().save.blocks!.toLocaleString()} blocks`}</dd>
          <dt>Modified</dt><dd>{when(entry().save.modified)}</dd>
          <dt>Checkpoints</dt><dd>{(history()?.length ?? entry().save.versions).toLocaleString()}</dd>
        </dl>
      </div>
      <h3 class="vp-section-title">Checkpoints</h3>
      <Show when={history()} fallback={<Throbber size="small" />}>
        <div class="vp-checkpoints" role="group" aria-label="Checkpoints"><For each={history()}>{(commit, index) =>
          <button type="button" class="vp-checkpoint" data-autofocus={index() === 0} aria-pressed={commit === chosen()} onFocus={() => setChosenId(commit.id)} onClick={() => setChosenId(commit.id)}>
            <Show when={commit.screenshot} fallback={<span class="vp-checkpoint-art"><SaveIcon save={entry().save} /></span>}>{shot => <img src={shot()} alt="" />}</Show>
            <span>{when(commit.timestamp)}</span><small>{index() === 0 ? "Current" : OPERATIONS[commit.operation]}</small>
          </button>
        }</For></div>
      </Show>
    </Modal>
    <ConfirmModal open={confirming()} onClose={() => setConfirming(false)} title="Restore checkpoint?" confirm="Restore"
      description={`${entry().save.title} is replaced with the checkpoint from ${chosen() ? when(chosen()!.timestamp) : ""}. The current save is kept as a checkpoint.`} onConfirm={() => void restore()} />
  </>}</Show>;
}

function Saves(props: { platform: string; volumes: ConsoleStorageVolume[]; busy: boolean; run(action: () => Promise<unknown>): Promise<void>; reload(): unknown }) {
  const [profileId, setProfileId] = createSignal<string | null>(null);
  const [open, setOpen] = createSignal<Entry | null>(null);
  const [menu, setMenu] = createSignal<"profile" | "copy" | "deleteProfile" | "deleteSave" | null>(null);
  const [naming, setNaming] = createSignal<Naming | null>(null);
  const profiles = () => props.volumes.filter(volume => volume.kind !== "battery");
  const batteries = () => props.volumes.filter(volume => volume.kind === "battery");
  const profile = () => profiles().find(volume => volume.id === profileId()) ?? profiles()[0];
  const [saves, { refetch }] = createResource<Entry[], ConsoleStorageVolume | ConsoleStorageVolume[]>(() => profile() ?? batteries(), async source => {
    const sources = Array.isArray(source) ? source : [source];
    return (await Promise.all(sources.map(async volume => (await volume.saves.list()).map(save => ({ key: `${volume.id}:${save.id}`, volume, save: { ...save, title: save.title.normalize("NFKC").trim() } }))))).flat();
  });
  const refresh = async (next?: string) => { await Promise.resolve(props.reload()); if (next) setProfileId(next); await refetch(); };
  const create = (then?: (volume: ConsoleStorageVolume) => Promise<unknown>) => setNaming({ title: "New profile", label: "", confirm: "Create", submit: label => props.run(async () => {
    const volume = await window.Polymer.consolestorage.Storage.create(profile()!.kind as "saveRepo" | "memoryCard", props.platform, { label });
    await then?.(volume);
    await refresh(volume.id);
  }) });
  const profileActions = (): MenuAction[] => profile() ? [
    { label: "New profile", run: () => create() },
    { label: `Rename ${profileName(profile())}`, run: () => setNaming({ title: "Rename profile", label: profileName(profile()), confirm: "Rename", submit: label => props.run(async () => { await profile()!.setLabel(label); await refresh(profile()!.id); }) }) },
    { label: `Duplicate ${profileName(profile())}`, run: () => setNaming({ title: "Duplicate profile", label: `${profileName(profile())} copy`, confirm: "Duplicate", submit: label => props.run(async () => refresh((await profile()!.fork(label)).id)) }) },
    { label: `Delete ${profileName(profile())}`, danger: true, disabled: profiles().length < 2, run: () => queueMicrotask(() => setMenu("deleteProfile")) },
  ] : [];
  const copyActions = (entry: Entry): MenuAction[] => [
    ...profiles().filter(volume => volume.id !== entry.volume.id).map(volume => ({ label: profileName(volume), run: () => void props.run(async () => { await entry.volume.saves.copyTo(entry.save.id, volume); setOpen(null); await refresh(volume.id); }) })),
    { label: "New profile", run: () => queueMicrotask(() => create(volume => entry.volume.saves.copyTo(entry.save.id, volume))) },
  ];
  const describe = (entry: Entry) => [entry.save.description, size(entry.save.byteLength), when(entry.save.modified), `${entry.save.versions} checkpoint${entry.save.versions === 1 ? "" : "s"}`].filter(Boolean).join(" · ");
  return <div class="vp-saves" data-saves-platform={props.platform}>
    <Show when={profiles().length > 1}><Tabs tabs={profiles().map(volume => ({ id: volume.id, title: profileName(volume) }))} active={profile()!.id} onChange={setProfileId} /></Show>
    <Show when={profile()}><div class="vp-actions"><Button onClick={() => setMenu("profile")}>{profileName(profile())} · Profile options</Button></div></Show>
    <Show when={saves()} fallback={<Throbber />}>{list => <Show when={list().length} fallback={<EmptyState title="No saves" body={profile() ? `${profileName(profile())} has no saves yet.` : "Saves appear here after you play a game."} />}>
      <For each={list()}>{entry => <Field label={entry.save.title} description={describe(entry)} icon={<SaveIcon save={entry.save} />} onActivate={() => setOpen(entry)} />}</For>
    </Show>}</Show>
    <SaveModal entry={menu() === "copy" || menu() === "deleteSave" ? null : open()} busy={props.busy} canCopy={!!profile() && open()?.volume.kind !== "battery"}
      subtitle={[open()?.volume.kind === "battery" ? null : profileName(open()?.volume), platformName(props.platform)].filter(Boolean).join(" · ")}
      onCopy={() => setMenu("copy")} onDelete={() => setMenu("deleteSave")} onClose={() => setOpen(null)}
      onRestore={commit => props.run(async () => { await open()!.volume.saves.restore(open()!.save.id, commit); await refetch(); })} />
    <ContextMenu open={menu() === "profile"} onClose={() => setMenu(current => current === "profile" ? null : current)} title={`Profiles · ${platformName(props.platform)}`} actions={profileActions()} />
    <Show when={open()}>{entry => <ContextMenu open={menu() === "copy"} onClose={() => setMenu(current => current === "copy" ? null : current)} title={`Copy ${entry().save.title} to`} actions={copyActions(entry())} />}</Show>
    <ConfirmModal open={menu() === "deleteProfile"} onClose={() => setMenu(null)} title={`Delete ${profileName(profile())}?`} description="All saves and checkpoints in this profile are permanently removed." confirm="Delete profile" danger
      onConfirm={() => void props.run(async () => { await profile()!.delete(); setProfileId(null); await refresh(); })} />
    <ConfirmModal open={menu() === "deleteSave"} onClose={() => setMenu(null)} title={`Delete ${open()?.save.title ?? ""}?`} description="The save is removed from this profile. Its checkpoints are kept." confirm="Delete save" danger
      onConfirm={() => void props.run(async () => { await open()!.volume.saves.delete(open()!.save.id); setOpen(null); await refresh(); })} />
    <NameModal request={naming()} busy={props.busy} onClose={() => setNaming(null)} />
  </div>;
}

export function SavesPage() {
  const shell = useShell();
  const [volumes, { refetch }] = createResource(async () => (await window.Polymer.consolestorage.Storage.list()).filter(volume => volume.kind !== "nand"));
  const [platform, setPlatform] = createSignal(shell.state.draft?.platform ?? "");
  const [busy, setBusy] = createSignal(false);
  const platforms = createMemo(() => [...new Set((volumes() ?? []).map(volume => volume.platform))]);
  const run = async (action: () => Promise<unknown>) => {
    if (busy()) return;
    setBusy(true);
    try { await action(); } catch (reason) { shell.fail(reason); } finally { setBusy(false); }
  };
  return <Page class="vp-settings" hints={[{ action: "activate", label: "Select" }, { action: "back", label: "Back" }]} onAction={action => {
    if (action.type !== "back") return false;
    shell.navigate("/");
    return true;
  }}>
    <Show when={volumes()} fallback={<EmptyState title="Loading save data"><Throbber /></EmptyState>}>
      <Show when={platforms().length} fallback={<EmptyState title="No save data" body="Saves appear here after you play a game." />}>
        <PagedSettings title="Saves" active={platforms().includes(platform()) ? platform() : platforms()[0]} onChange={setPlatform} pages={platforms().map(system => ({
          id: system, title: platformName(system), icon: "disc" as const,
          content: () => <Saves platform={system} volumes={volumes()!.filter(volume => volume.platform === system)} busy={busy()} run={run} reload={refetch} />,
        }))} />
      </Show>
    </Show>
  </Page>;
}
