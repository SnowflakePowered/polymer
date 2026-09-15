import { For, Show, createSignal, onMount } from "solid-js";
import { useSearchParams } from "@solidjs/router";
import type { ConsoleStorageVolume, StorageInspection, StorageRemovalPlan } from "../../../../../../js/polymer";
import { AppSidebar } from "../../components/AppSidebar";
import { useShell } from "../../app/shell-context";
import { friendly } from "../../app/launch";
import { selectNand, selectedNand } from "./selection";
import "./StoragePage.css";

const nandPlatforms = ["NINTENDO_WII", "NINTENDO_WIIU", "NINTENDO_3DS", "NINTENDO_N3DS", "SONY_PS3", "SONY_PSP", "SONY_PSV"];

export function StoragePage() {
  const shell = useShell();
  const [query] = useSearchParams();
  const [platforms, setPlatforms] = createSignal<string[]>([]);
  const [platform, setPlatform] = createSignal(query.platform || shell.model.draft?.platform || "NINTENDO_WII");
  const [volumes, setVolumes] = createSignal<ConsoleStorageVolume[]>([]);
  const [currentId, setCurrentId] = createSignal(selectedNand(platform()) || "");
  const current = () => volumes().find(volume => volume.id === currentId() && volume.platform === platform());
  const [inspection, setInspection] = createSignal<StorageInspection>();
  const [label, setLabel] = createSignal("");
  const [source, setSource] = createSignal("");
  const [plan, setPlan] = createSignal<StorageRemovalPlan>();
  const [confirmDelete, setConfirmDelete] = createSignal(false);
  const [confirmRecover, setConfirmRecover] = createSignal(false);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const api = window.Polymer.consolestorage;
  const refresh = async () => {
    const items = await api.Storage.list({ kind: "nand" });
    setVolumes(items);
    const volume = items.find(item => item.id === currentId());
    setInspection(volume ? await volume.inspect() : undefined);
  };
  const run = async (action: () => Promise<unknown>) => {
    if (busy()) return;
    setBusy(true); setError("");
    try { await action(); }
    catch (reason) { setError(reason instanceof Error ? reason.message : String(reason)); }
    finally {
      try { await refresh(); }
      catch (reason) { if (!error()) setError(reason instanceof Error ? reason.message : String(reason)); }
      setBusy(false);
    }
  };
  const choose = (id: string) => {
    setCurrentId(id); setPlan(undefined); setConfirmDelete(false); setConfirmRecover(false); setInspection(undefined);
    const volume = volumes().find(item => item.id === id);
    if (volume) selectNand(volume);
    void run(refresh);
  };
  const changePlatform = (value: string) => {
    setPlatform(value); setSource("");
    choose(selectedNand(value) || "");
  };
  const create = async () => {
    const volume = await api.Storage.create("nand", platform(), { label: label().trim() || `${friendly(platform())} NAND` });
    setCurrentId(volume.id); selectNand(volume); setPlan(undefined);
    if (platforms().includes(platform())) {
      await volume.bootstrap();
    }
    setLabel("");
  };
  const remove = async () => {
    const volume = current(), removal = plan();
    if (!volume || !removal || removal.affected.length) return;
    await volume.artifacts.uninstall(removal);
    setPlan(undefined);
  };
  onMount(() => {
    void api.platforms().then(items => setPlatforms(items.map(item => item.platform))).catch(reason => setError(String(reason)));
    void run(refresh);
  });

  return <main class="horizon-storage-shell"><AppSidebar /><section class="horizon-storage">
    <h1>Console storage</h1>
    <Show when={shell.model.draft && shell.model.launcher.kind === "error"}><p>{shell.model.error}</p></Show>
    <Show when={error()}><p role="alert" class="storage-error">{error()}</p></Show>
    <label>Platform<select value={platform()} disabled={busy()} onChange={event => changePlatform(event.currentTarget.value)}>
      <For each={[...new Set([...nandPlatforms, ...platforms(), ...volumes().map(volume => volume.platform), platform()])]}>{id => <option value={id}>{friendly(id)}</option>}</For>
    </select></label>
    <div class="storage-columns"><section>
      <h2>Your NANDs</h2>
      <label>Selected NAND<select value={currentId()} disabled={busy()} onChange={event => choose(event.currentTarget.value)}>
        <option value="">Choose a NAND</option>
        <For each={volumes().filter(volume => volume.platform === platform())}>{volume => <option value={volume.id}>{volume.label || volume.id}</option>}</For>
      </select></label>
      <Show when={current()}>{volume => <>
        <Show when={inspection()?.dirty}>
          <div role="alert" class="storage-confirm">
            <p>This NAND has uncommitted changes. Recover it before launching or installing contents.</p>
            <button disabled={busy()} onClick={() => setConfirmRecover(true)}>Recover NAND…</button>
          </div>
        </Show>
        <Show when={confirmRecover()}><div class="storage-confirm">
          <p>Discard all uncommitted changes to “{volume().label || volume().id}”? Successfully installed contents and separate save profiles are kept.</p>
          <button disabled={busy()} onClick={() => void run(async () => { await volume().recover(); setConfirmRecover(false); setPlan(undefined); })}>Discard changes and recover</button>
          <button disabled={busy()} onClick={() => setConfirmRecover(false)}>Cancel</button>
        </div></Show>
        <p>{inspection()?.minimal ? "Basic system contents are present. Launch requirements are checked when starting a game." : "Setup is incomplete. Install system contents or complete setup."}</p>
        <div class="storage-actions">
          <Show when={platforms().includes(platform())}><button disabled={busy() || inspection()?.dirty} onClick={() => void run(async () => { await volume().bootstrap(); })}>Complete setup</button></Show>
          <button disabled={busy()} onClick={() => setConfirmDelete(true)}>Delete NAND…</button>
          <Show when={shell.model.draft?.platform === platform()}><button disabled={busy() || inspection()?.dirty} onClick={() => shell.launchDraft()}>Continue launch</button></Show>
        </div>
        <Show when={confirmDelete()}><div role="alert" class="storage-confirm">
          <p>Delete “{volume().label || volume().id}” and its installed contents? Separate save profiles are kept.</p>
          <button disabled={busy()} onClick={() => void run(async () => { await volume().delete(); localStorage.removeItem(`polymer.nand.${platform()}`); setCurrentId(""); setConfirmDelete(false); })}>Delete NAND</button>
          <button onClick={() => setConfirmDelete(false)}>Cancel</button>
        </div></Show>
        <h2>Install contents</h2>
        <label>File or directory path<input value={source()} onInput={event => setSource(event.currentTarget.value)} /></label>
        <button disabled={busy() || inspection()?.dirty || !source().trim()} onClick={() => void run(async () => { await volume().install({ path: source().trim() }); })}>Install</button>
        <h2>Installed artifacts</h2>
        <For each={inspection()?.artifacts || []}>{artifact => <div class="storage-artifact"><span>{artifact.label} {artifact.version || ""}</span><button disabled={busy()} onClick={() => void run(async () => setPlan(await volume().artifacts.planRemoval([artifact.id])))}>Remove…</button></div>}</For>
        <Show when={plan()}>{removal => <div class="storage-confirm">
          <p>Remove {removal().requested.join(", ")}?</p>
          <Show when={removal().affected.length}><p>Also affects: {removal().affected.join(", ")}. Review these artifacts before including them.</p><button onClick={() => void run(async () => setPlan(await volume().artifacts.planRemoval([...removal().requested, ...removal().affected])))}>Include affected artifacts</button></Show>
          <button disabled={busy() || inspection()?.dirty || !!removal().affected.length} onClick={() => void run(remove)}>Remove selected artifacts</button><button onClick={() => setPlan(undefined)}>Cancel</button>
        </div>}</Show>
      </>}</Show>
    </section><section>
      <h2>Add NAND</h2>
      <label>Name<input value={label()} onInput={event => setLabel(event.currentTarget.value)} placeholder={`${friendly(platform())} NAND`} /></label>
      <Show when={platforms().includes(platform())} fallback={<p>Create an empty NAND, then install system contents using a supported format.</p>}>
        <p>Download and install the system files provided by this platform's plugin.</p>
      </Show>
      <button disabled={busy()} onClick={() => void run(create)}>Create NAND</button>
      <Show when={busy()}><p aria-live="polite">Working…</p><progress aria-label="Storage operation" /></Show>
    </section></div>
  </section></main>;
}
