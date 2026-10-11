import { For, Show, createResource, createSignal, createEffect, on } from "solid-js";
import type { ConsoleStorageVolume, SaveCommitOperation, StorageSave } from "@polymer-fe/frontend";
import { css } from "../../../styled-system/css";
import { Dialog, ConfirmDialog, Button, TextField, Spinner, ErrorState, DetailList, Icon, BannerImage } from "../../ui";

export type NameRequest = { title: string; label: string; confirm: string; submit(label: string): Promise<unknown> };

export const when = (timestamp: string) => {
  const date = new Date(timestamp), days = Math.round((new Date().setHours(0, 0, 0, 0) - new Date(timestamp).setHours(0, 0, 0, 0)) / 86400000);
  const time = date.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  return days === 0 ? `Today, ${time}` : days === 1 ? `Yesterday, ${time}` : `${date.toLocaleDateString()}, ${time}`;
};

const operations: Record<SaveCommitOperation, string> = { initialImport: "Imported", autosave: "Autosave", manual: "Manual", restore: "Restored" };

const detailsDialog = css({ width: "min(46rem, 100%)" });
const overview = css({ display: "flex", gap: "1.5rem", alignItems: "center", marginBottom: "1.25rem", "& .hz-details": { flex: 1, minWidth: 0 } });
const banner = css({ width: "9rem", height: "9rem", flex: "none", borderRadius: "card", objectFit: "contain", display: "flex", alignItems: "center", justifyContent: "center", bg: "panel", "& svg": { width: "3.5rem", height: "3.5rem" } });
const heading = css({ margin: "0 0 .5rem", fontSize: "1.1rem", fontWeight: "400", color: "muted" });
const carousel = css({ display: "flex", gap: ".75rem", width: 0, minWidth: "100%", overflowX: "auto", overflowY: "hidden", padding: "var(--hz-focus-width)" });
const card = css({ display: "flex", flexDirection: "column", flex: "none", width: "9rem", gap: ".35rem", padding: ".5rem", border: "0", borderRadius: "card", bg: "panel", color: "text", textAlign: "center", font: "inherit", cursor: "pointer", "& small": { color: "muted" }, "&[aria-pressed=true]": { bg: "raised" } });
const cardImage = css({ width: "100%", height: "5rem", borderRadius: "control", objectFit: "contain", display: "flex", alignItems: "center", justifyContent: "center", bg: "overlay", "& svg": { width: "2rem", height: "2rem" } });

const kilobytes = (length: number) => `${Math.max(1, Math.ceil(length / 1024)).toLocaleString()} KB`;

export function SaveIcon(props: { save: StorageSave; class: string }) {
  return <Show when={props.save.icon} fallback={<span class={props.class}><Icon name="saves" tone="muted" strokeWidth={1.2} /></span>}>{icon => <BannerImage class={props.class} src={icon()} />}</Show>;
}

export function ProfileNameDialog(props: { request: NameRequest | null; pending: boolean; returnFocus(): HTMLElement | null; onClose(): void }) {
  const [label, setLabel] = createSignal("");
  createEffect(() => setLabel(props.request?.label ?? ""));
  const submit = async () => {
    if (!label().trim()) return;
    await props.request!.submit(label().trim());
    props.onClose();
  };
  return <Dialog open={!!props.request} pending={props.pending} returnFocus={props.returnFocus} title={props.request?.title ?? ""} onOpenChange={open => { if (!open) props.onClose(); }}
    actions={<><Button disabled={props.pending} onClick={props.onClose}>Cancel</Button><Button appearance="primary" pending={props.pending} disabled={!label().trim()} onClick={() => void submit()}>{props.request?.confirm}</Button></>}>
    <form onSubmit={event => { event.preventDefault(); void submit(); }}><TextField label="Profile name" value={label()} onValueChange={setLabel} /></form>
  </Dialog>;
}

export function SaveDetailsDialog(props: { open: boolean; entry: { volume: ConsoleStorageVolume; save: StorageSave }; subtitle: string; pending: boolean; canFork: boolean; returnFocus(): HTMLElement | null; onFork(): void; onDelete(): void; onRestore(commit: string): Promise<unknown>; onClose(): void }) {
  const [history, { refetch }] = createResource(() => props.open && props.entry, async entry => (await entry.volume.saves.history(entry.save.id)).sort((a, b) => Date.parse(b.timestamp) - Date.parse(a.timestamp)));
  const [chosenId, setChosenId] = createSignal<string | null>(null);
  const [confirming, setConfirming] = createSignal(false);
  let strip: HTMLDivElement | undefined;
  createEffect(on(() => props.entry, () => {
    setChosenId(null);
    if (strip) strip.scrollLeft = 0;
  }));
  const chosen = () => history()?.find(commit => commit.id === chosenId()) ?? history()?.[0];
  const current = () => history()?.[0];
  const save = () => props.entry.save;
  const chosenCard = () => strip?.querySelector<HTMLElement>(`[data-commit="${chosen()?.id ?? ""}"]`) ?? null;
  const restore = async () => {
    const commit = chosen();
    if (!commit) return;
    await props.onRestore(commit.id);
    await refetch();
    setChosenId(commit.id);
    setConfirming(false);
    queueMicrotask(() => chosenCard()?.focus());
  };
  return <>
    <Dialog class={detailsDialog} open={props.open} pending={props.pending} returnFocus={props.returnFocus} title={save().title} description={props.subtitle} onOpenChange={open => { if (!open) props.onClose(); }}
      actions={<>
        <Show when={props.canFork}><Button disabled={props.pending} onClick={props.onFork}>Copy to Profile</Button></Show>
        <Button disabled={props.pending || !chosen() || chosen() === current()} onClick={() => setConfirming(true)}>Restore Checkpoint</Button>
        <Button appearance="danger" disabled={props.pending} onClick={props.onDelete}>Delete Save</Button>
      </>}>
      <div class={overview}>
        <SaveIcon save={save()} class={banner} />
        <DetailList items={[
          { label: "Size", value: kilobytes(save().byteLength) },
          ...save().blocks === null ? [] : [{ label: "Blocks", value: save().blocks!.toLocaleString() }],
          { label: "Modified", value: when(save().modified) },
          { label: "Checkpoints", value: (history()?.length ?? save().versions).toLocaleString() },
        ]} />
      </div>
      <h3 class={heading}>Checkpoints</h3>
      <Show when={!history.error} fallback={<ErrorState message={String(history.error)} />}>
        <Show when={history()} fallback={<Spinner label="Loading checkpoints" />}>
          <div ref={strip} class={carousel} role="group" aria-label="Checkpoints"><For each={history()}>{(commit, index) => <button type="button" class={card} data-select-ring data-commit={commit.id} aria-pressed={commit === chosen()} onFocus={() => setChosenId(commit.id)} onClick={() => setChosenId(commit.id)}>
            <Show when={commit.screenshot} fallback={<SaveIcon save={save()} class={cardImage} />}>{screenshot => <img class={cardImage} src={screenshot()} alt="" />}</Show>
            <span>{when(commit.timestamp)}</span>
            <small>{index() === 0 ? "Current" : operations[commit.operation]}</small>
          </button>}</For></div>
        </Show>
      </Show>
    </Dialog>
    <ConfirmDialog open={confirming()} pending={props.pending} title="Restore checkpoint?" description={`${save().title} is replaced with the checkpoint from ${chosen() ? when(chosen()!.timestamp) : ""}. The current save is kept as a checkpoint.`} confirmLabel="Restore Checkpoint" returnFocus={chosenCard} onClose={() => setConfirming(false)} onConfirm={() => void restore()} />
  </>;
}
