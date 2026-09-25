import { For, Show, createResource, createSignal, onCleanup, onMount } from "solid-js";
import { useSearchParams } from "@solidjs/router";
import type { ConsoleStorageVolume, DepotType, StorageArtifact, StorageInspection, StorageInstall, StorageRemovalPlan } from "../../../../../../js/polymer";
import { css } from "../../../styled-system/css";
import { Screen, ScreenHeader, MasterDetail, CategoryNavigation, TileGrid, Tile, Icon, DetailList, Section, Button, ActionGroup, Dialog, ConfirmDialog, ActionHints, EmptyState, ErrorState, Progress, Spinner } from "../../ui";
import { AppSidebar } from "../../components/AppSidebar";
import { useShell } from "../../app/shell-context";
import { selectedNand, installPhases } from "./selection";
import "./StoragePage.css";

const depotTypes: Record<DepotType, string> = { system: "System title", app: "Application", appcontent: "Downloadable content", appupdate: "Update" };
const megabytes = (bytes: number) => `${(bytes / 1048576).toFixed(1)} MB`;
const installedOn = (artifact: StorageArtifact) => Date.parse(artifact.installed) > 0 ? new Date(artifact.installed).toLocaleDateString() : "Unknown";

const titleGrid = css({ gridTemplateColumns: "repeat(auto-fill, minmax(8.5rem, 1fr))", gap: "1rem" });
const titleTile = css({ display: "flex", flexDirection: "column", gap: ".4rem", padding: "var(--hz-focus-width)", bg: "transparent", borderRadius: "card", aspectRatio: "1", "&[data-selected]": { boxShadow: "none" } });
const titleIcon = css({ width: "100%", flex: 1, minHeight: 0, borderRadius: "control", objectFit: "cover", display: "flex", alignItems: "center", justifyContent: "center", bg: "overlay", "& svg": { width: "3rem", height: "3rem" } });
const detailIcon = css({ width: "100%", maxWidth: "12rem", aspectRatio: "1", borderRadius: "card", objectFit: "cover", display: "flex", alignItems: "center", justifyContent: "center", bg: "panel", "& svg": { width: "5rem", height: "5rem" } });

const installTitle = (install: StorageInstall) => install.state === "running" ? install.detail.title : null;

function TitleIcon(props: { artifact: StorageArtifact; class: string }) {
  return <Show when={props.artifact.icon} fallback={<span class={props.class}><Icon name={props.artifact.depotType === "system" ? "settings" : "gamepad"} tone="muted" strokeWidth={1.2} /></span>}>{icon => <img class={props.class} src={icon()} alt="" />}</Show>;
}

export function StoragePage() {
  const shell = useShell();
  const [query] = useSearchParams();
  const [platforms, setPlatforms] = createSignal<string[]>([]);
  const [platform, setPlatform] = createSignal(query.platform || shell.model.draft?.platform || "");
  const [systems, setSystems] = createSignal<string[]>();
  const [volumes, setVolumes] = createSignal<ConsoleStorageVolume[]>([]);
  const [currentId, setCurrentId] = createSignal("");
  const current = () => volumes().find(volume => volume.id === currentId() && volume.platform === platform()) ?? volumes().find(volume => volume.platform === platform());
  const [plan, setPlan] = createSignal<StorageRemovalPlan>();
  const [confirmRecover, setConfirmRecover] = createSignal(false);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const api = window.Polymer.consolestorage;
  const [installs, setInstalls] = createSignal(api.installs);
  const [inspection, { refetch: reinspect }] = createResource<StorageInspection | undefined, ConsoleStorageVolume>(current, volume => volume.inspect().catch(reason => { setError(reason instanceof Error ? reason.message : String(reason)); return undefined; }));
  const checked = () => current() && !inspection.loading ? inspection() : undefined;
  const loadVolumes = async () => {
    const items = await api.Storage.list({ kind: "nand" });
    setVolumes(items);
    setSystems([...new Set([...items.map(volume => volume.platform), ...installs().map(install => install.platform)])]);
    if (!platform() && systems()!.length) changePlatform(systems()![0]);
  };
  const refresh = async () => {
    await loadVolumes();
    if (current()) await reinspect();
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
  const changePlatform = (value: string) => {
    setPlatform(value);
    setCurrentId(""); setPlan(undefined); setConfirmRecover(false);
    void selectedNand(value).then(id => { if (platform() === value) setCurrentId(id || ""); });
  };
  const remove = async () => {
    const volume = current(), removal = plan();
    if (!volume || !removal || removal.affected.length) return;
    await volume.artifacts.uninstall(removal);
    setPlan(undefined);
  };
  onMount(() => {
    const listener = (event: Event) => {
      const install = (event as CustomEvent<StorageInstall>).detail;
      setInstalls(api.installs);
      if (install.state === "queued" && !systems()?.includes(install.platform)) void loadVolumes();
      if (install.state === "complete" || install.state === "failed") void refresh();
    };
    api.addEventListener("installchange", listener);
    onCleanup(() => api.removeEventListener("installchange", listener));
    void api.platforms().then(items => setPlatforms(items.map(item => item.platform))).catch(reason => setError(String(reason)));
    const initial = platform();
    if (initial) void selectedNand(initial).then(id => { if (platform() === initial && !currentId()) setCurrentId(id || ""); });
    void loadVolumes().catch(reason => setError(String(reason)));
  });

  const [artifactId, setArtifactId] = createSignal<string | null>(null);
  const pending = () => installs().filter(install => install.storage === current()?.id);
  const artifact = () => checked()?.artifacts.find(item => item.id === artifactId()) ?? checked()?.artifacts[0];
  const systemItems = () => [...new Set([...systems() ?? [], ...platform() ? [platform()] : []])].map(value => ({ value, label: window.Polymer.machine(value)?.name ?? value }));
  return <div class="horizon-storage-shell"><AppSidebar /><Screen class="horizon-storage" hints={[{ action: "move", label: "Change pane" }, { action: "back", label: "Back" }, { action: "activate", label: "Select" }]} onAction={action => { if (action.type !== "back") return false; shell.navigate("/"); return true; }}>
    <ScreenHeader title="System Storage" />
    <Show when={shell.model.draft && shell.model.launcher.kind === "error"}><ErrorState message={shell.model.error} /></Show>
    <Show when={error()}><ErrorState message={error()} /></Show><Show when={busy()}><Progress label="Storage operation" /></Show>
    <Show when={systems()} fallback={<Spinner label="Checking NANDs" />}><Show when={systemItems().length} fallback={<EmptyState title="No titles installed" description="Drag a title package onto your library to install it." />}>
    <MasterDetail navigation={<CategoryNavigation label="Systems" value={platform()} items={systemItems()} onValueChange={value => { if (value !== platform()) changePlatform(value); }} />}
      detail={<Show when={artifact()}>{item => <Section title={item().label}>
        <TitleIcon artifact={item()} class={detailIcon} />
        <DetailList items={[{ label: "Title ID", value: item().id }, { label: "Type", value: depotTypes[item().depotType] }, { label: "Version", value: item().version || "—" }, { label: "Installed size", value: megabytes(item().byteLength) }, { label: "Files", value: String(item().fileCount) }, { label: "Installed", value: installedOn(item()) }]} />
        <Button appearance="danger" disabled={busy() || checked()?.dirty} onClick={() => void run(async () => setPlan(await current()!.artifacts.planRemoval([item().id])))}>Uninstall</Button>
      </Section>}</Show>}>

      <Show when={current()}>{volume => <>
          <Show when={checked()?.dirty}><ErrorState message="This NAND has uncommitted changes. Recover it before launching or installing contents."><Button disabled={busy()} onClick={() => setConfirmRecover(true)}>Recover NAND…</Button></ErrorState></Show>
          <Show when={platforms().includes(platform()) || shell.model.draft?.platform === platform()}><ActionGroup>
            <Show when={platforms().includes(platform())}><Button disabled={busy() || checked()?.dirty} onClick={() => void run(async () => { await volume().bootstrap(); })}>Complete setup</Button></Show>
            <Show when={shell.model.draft?.platform === platform()}><Button disabled={busy() || checked()?.dirty} onClick={() => shell.launchDraft()}>Continue launch</Button></Show>
          </ActionGroup></Show>
          <Section title="Installed titles"><Show when={checked()?.artifacts.length || pending().length} fallback={<Show when={checked()} fallback={<Spinner label="Checking NAND" />}><EmptyState title="No titles installed" description="Drag a title package onto your library to install it." /></Show>}>
            <TileGrid class={titleGrid} aria-label="Installed titles"><For each={pending()}>{install => <Tile class={titleTile} aria-label={`Installing ${installTitle(install)?.label ?? install.filename}`}>
              <Show when={installTitle(install)?.icon} fallback={<span class={titleIcon}><Icon name="gamepad" tone="muted" strokeWidth={1.2} /></span>}>{icon => <img class={titleIcon} src={icon()} alt="" />}</Show>
              <Progress label={install.state === "running" ? installPhases[install.detail.phase] : "Waiting to install"} completed={install.state === "running" ? install.detail.completed : undefined} total={install.state === "running" ? install.detail.total : undefined} />
            </Tile>}</For><For each={checked()?.artifacts ?? []}>{item => <Tile class={titleTile} selected={artifact()?.id === item.id} aria-label={item.label} onFocus={() => setArtifactId(item.id)} onClick={() => setArtifactId(item.id)}>
              <TitleIcon artifact={item} class={titleIcon} />
            </Tile>}</For></TileGrid>
          </Show></Section>
      </>}</Show>
    </MasterDetail></Show></Show><ActionHints />
    <ConfirmDialog open={confirmRecover()} pending={busy()} title="Recover NAND?" description={`Discard uncommitted changes to ${current()?.label || currentId()}? Successfully installed contents and separate save profiles are kept.`} confirmLabel="Discard changes and recover" onClose={() => setConfirmRecover(false)} onConfirm={() => void run(async () => { await current()!.recover(); setConfirmRecover(false); setPlan(undefined); })} />
    <Dialog open={!!plan()} onOpenChange={open => { if (!open && !busy()) setPlan(undefined); }} pending={busy()} title="Remove installed titles" actions={<><Button disabled={busy()} onClick={() => setPlan(undefined)}>Cancel</Button><Button appearance="danger" pending={busy()} disabled={checked()?.dirty || !!plan()?.affected.length} onClick={() => void run(remove)}>Remove selected artifacts</Button></>}>
      <Show when={plan()}>{removal => <><p>Remove {removal().requested.join(", ")}?</p><Show when={removal().affected.length}><p>Also affects: {removal().affected.join(", ")}. Review these artifacts before including them.</p><Button disabled={busy()} onClick={() => void run(async () => setPlan(await current()!.artifacts.planRemoval([...removal().requested, ...removal().affected])))}>Include affected artifacts</Button></Show></>}</Show><Show when={error()}><ErrorState message={error()} /></Show>
    </Dialog>
  </Screen></div>;
}
