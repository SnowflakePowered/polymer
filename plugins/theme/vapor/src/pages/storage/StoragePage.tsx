import { For, Show, createResource, createSignal, onCleanup, onMount } from "solid-js";
import { useSearchParams } from "@solidjs/router";
import type { ConsoleStorageVolume, DepotType, Job, StorageArtifact, StorageInspection, StorageRemovalPlan } from "@polymer-fe/frontend";
import { useShell } from "../../app/shell";
import { Button, ConfirmModal, EmptyState, Field, Icon, Modal, Page, PagedSettings, ProgressBar, Throbber } from "../../ui";
import { platformName } from "../library/platform";

const DEPOTS: Record<DepotType, string> = { system: "System title", app: "Application", appcontent: "Downloadable content", appupdate: "Update" };
const size = (bytes: number) => bytes >= 1073741824 ? `${(bytes / 1073741824).toFixed(2)} GB` : `${(bytes / 1048576).toFixed(1)} MB`;
const installing = (job: Job) => job.kind === "install" && (job.state === "queued" || job.state === "running");
const message = (reason: unknown) => reason instanceof Error ? reason.message : String(reason);

function TitleIcon(props: { icon?: string; system?: boolean }) {
  return <Show when={props.icon} fallback={<Icon name={props.system ? "settings" : "controller"} color="#67707b" />}>{icon => <img src={icon()} alt="" />}</Show>;
}

function Volume(props: { volume: ConsoleStorageVolume; installs: Job[]; bootstrap: boolean; busy: boolean; run(action: () => Promise<unknown>): Promise<void>; revision: number }) {
  const shell = useShell();
  const [plan, setPlan] = createSignal<StorageRemovalPlan>();
  const [recovering, setRecovering] = createSignal(false);
  const [inspection, { refetch }] = createResource(() => [props.volume, props.revision] as const, ([volume]) => volume.inspect());
  const checked = (): StorageInspection | undefined => inspection.loading ? undefined : inspection();
  const total = () => (checked()?.artifacts ?? []).reduce((sum, artifact) => sum + artifact.byteLength, 0);
  const run = (action: () => Promise<unknown>) => props.run(async () => { await action(); await refetch(); });
  const remove = () => run(async () => { const removal = plan(); if (removal && !removal.affected.length) await props.volume.artifacts.uninstall(removal); setPlan(undefined); });
  const describe = (artifact: StorageArtifact) => [DEPOTS[artifact.depotType], artifact.version && `Version ${artifact.version}`, `${artifact.fileCount.toLocaleString()} files`,
    Date.parse(artifact.installed) > 0 && `Installed ${new Date(artifact.installed).toLocaleDateString()}`].filter(Boolean).join(" · ");
  return <>
    <Show when={checked()?.dirty}><div class="vp-notice"><p>This NAND has uncommitted changes. Recover it before launching or installing content.</p><Button disabled={props.busy} onClick={() => setRecovering(true)}>Recover NAND</Button></div></Show>
    <Field label={props.volume.label ?? "Internal NAND"} description={inspection.error ? message(inspection.error) : `${checked()?.artifacts.length ?? "…"} titles · ${size(total())}`} icon={<Icon name="storage" />}>
      <Show when={inspection.loading}><Throbber size="small" /></Show>
    </Field>
    <Show when={props.bootstrap || shell.state.draft?.platform === props.volume.platform}><div class="vp-actions">
      <Show when={props.bootstrap}><Button disabled={props.busy || checked()?.dirty} onClick={() => void run(() => props.volume.bootstrap())}>Complete setup</Button></Show>
      <Show when={shell.state.draft?.platform === props.volume.platform}><Button primary disabled={props.busy || checked()?.dirty} onClick={() => void shell.launch()}>Continue launch</Button></Show>
    </div></Show>
    <Show when={props.installs.length}><h3 class="vp-section-title">Installing</h3><For each={props.installs}>{install =>
      <Field label={install.title} icon={<TitleIcon icon={install.icon ?? undefined} />} childrenBelow>
        <ProgressBar label={install.message ?? "Waiting to install"} value={install.total ? (install.completed ?? 0) / install.total : undefined} detail={install.total ? `${Math.round((install.completed ?? 0) / install.total * 100)}%` : undefined} />
      </Field>
    }</For></Show>
    <h3 class="vp-section-title">Installed titles</h3>
    <Show when={checked()} fallback={<Throbber />}>{inspected => <Show when={inspected().artifacts.length} fallback={<EmptyState title="No titles installed" body="Drop a title package onto the library to install it." />}>
      <For each={inspected().artifacts}>{artifact => <Field label={artifact.label} description={describe(artifact)} icon={<TitleIcon icon={artifact.icons[0]} system={artifact.depotType === "system"} />}>
        <span class="vp-storage-size">{size(artifact.byteLength)}</span>
        <Button danger disabled={props.busy || inspected().dirty} onClick={() => void run(async () => setPlan(await props.volume.artifacts.planRemoval([artifact.id])))}>Uninstall</Button>
      </Field>}</For>
    </Show>}</Show>
    <ConfirmModal open={recovering()} title="Recover NAND?" description={`Discard uncommitted changes to ${props.volume.label ?? platformName(props.volume.platform)}? Installed content and save profiles are kept.`} confirm="Discard and recover" danger
      onClose={() => setRecovering(false)} onConfirm={() => void run(() => props.volume.recover())} />
    <Modal open={!!plan()} onClose={() => { if (!props.busy) setPlan(undefined); }} title="Uninstall" description={plan() ? `Remove ${plan()!.requested.join(", ")} from this NAND?` : ""}
      buttons={<><Button danger data-autofocus disabled={props.busy || !!plan()?.affected.length} onClick={() => void remove()}>Uninstall</Button><Button disabled={props.busy} onClick={() => setPlan(undefined)}>Cancel</Button></>}>
      <Show when={plan()?.affected.length}>
        <p class="vp-modal-body">These titles depend on it and must be removed too: {plan()!.affected.join(", ")}.</p>
        <div class="vp-actions"><Button disabled={props.busy} onClick={() => void run(async () => setPlan(await props.volume.artifacts.planRemoval([...plan()!.requested, ...plan()!.affected])))}>Include dependent titles</Button></div>
      </Show>
    </Modal>
  </>;
}

export function StoragePage() {
  const shell = useShell();
  const [query] = useSearchParams<{ platform?: string }>();
  const [platform, setPlatform] = createSignal(query.platform ?? shell.state.draft?.platform ?? "");
  const [volumes, setVolumes] = createSignal<ConsoleStorageVolume[]>();
  const [bootstrappable, setBootstrappable] = createSignal<string[]>([]);
  const [installs, setInstalls] = createSignal<Job[]>([]);
  const [busy, setBusy] = createSignal(false);
  const [revision, setRevision] = createSignal(0);
  const load = async () => setVolumes(await window.Polymer.consolestorage.Storage.list({ kind: "nand" }));
  const run = async (action: () => Promise<unknown>) => {
    if (busy()) return;
    setBusy(true);
    try { await action(); } catch (reason) { shell.fail(reason); } finally { setBusy(false); await load().catch(shell.fail); }
  };
  const systems = () => [...new Set([...(volumes() ?? []).map(volume => volume.platform), ...installs().flatMap(job => job.subject.platform ?? []), ...platform() ? [platform()] : []])];
  onMount(() => {
    const changed = (event: Event) => {
      const job = (event as CustomEvent<Job>).detail;
      if (job.kind !== "install") return;
      setInstalls(jobs => [...jobs.filter(item => item.id !== job.id), ...installing(job) ? [job] : []]);
      if (!installing(job)) { setRevision(value => value + 1); void load().catch(shell.fail); }
    };
    window.Polymer.jobs.addEventListener("jobchange", changed);
    onCleanup(() => window.Polymer.jobs.removeEventListener("jobchange", changed));
    void window.Polymer.jobs.list().then(jobs => setInstalls(jobs.filter(installing))).catch(shell.fail);
    void window.Polymer.consolestorage.platforms().then(items => setBootstrappable(items.map(item => item.platform))).catch(shell.fail);
    void load().catch(shell.fail);
  });
  return <Page class="vp-settings" hints={[{ action: "activate", label: "Select" }, { action: "back", label: "Back" }]} onAction={action => {
    if (action.type !== "back") return false;
    shell.navigate("/");
    return true;
  }}>
    <Show when={volumes()} fallback={<EmptyState title="Checking system storage"><Throbber /></EmptyState>}>
      <Show when={systems().length} fallback={<EmptyState title="No system storage" body="System storage is created when you install a title or set up a console." />}>
        <PagedSettings title="System Storage" active={platform() || systems()[0]} onChange={setPlatform} pages={systems().map(system => ({
          id: system, title: platformName(system), icon: "storage" as const,
          content: () => <Show when={volumes()!.find(volume => volume.platform === system)} fallback={<EmptyState title="Waiting for installation" body="This system's storage appears when its first install finishes." />}>{volume =>
            <Volume volume={volume()} installs={installs().filter(job => job.subject.storage === volume().id || job.subject.platform === system)} bootstrap={bootstrappable().includes(system)} busy={busy()} run={run} revision={revision()} />
          }</Show>,
        }))} />
      </Show>
    </Show>
  </Page>;
}
