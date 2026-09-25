import { Progress as ArkProgress } from "@ark-ui/solid";
import { Show, type ParentProps } from "solid-js";
import { IconButton } from "./actions";

export function Progress(props: { label: string; completed?: number; total?: number }) {
  const value = () => props.completed !== undefined && props.total && props.total > 0 ? Math.max(0, Math.min(100, props.completed / props.total * 100)) : null;
  return <ArkProgress.Root class="hz-progress" value={value()}><ArkProgress.Label class="hz-visually-hidden">{props.label}</ArkProgress.Label><ArkProgress.Track><ArkProgress.Range /></ArkProgress.Track></ArkProgress.Root>;
}
export function Spinner(props: { label?: string }) { return <span class="hz-spinner" role="status" aria-label={props.label ?? "Loading"} />; }
export function Badge(props: ParentProps<{ danger?: boolean }>) { return <span class="hz-badge" data-danger={props.danger || undefined}>{props.children}</span>; }
export function Toast(props: ParentProps<{ title: string; message: string; icon?: string; error?: boolean; onDismiss: () => void }>) {
  return <section class="hz-toast" data-error={props.error || undefined} role={props.error ? "alert" : "status"}><Show when={props.icon}>{icon => <img class="hz-toast-icon" src={icon()} alt="" />}</Show><div><strong>{props.title}</strong><p>{props.message}</p>{props.children}</div><IconButton icon="close" label={`Dismiss ${props.title}`} onClick={props.onDismiss} /></section>;
}
export function JobIndicator(props: { title: string; message: string; completed?: number; total?: number; onOpen: () => void }) {
  return <button type="button" class="hz-job" aria-label={`${props.title}: ${props.message}`} title={`${props.title}: ${props.message}`} onClick={props.onOpen}><span>{props.title}</span><Progress label={props.message} completed={props.completed} total={props.total} /></button>;
}
export function EmptyState(props: ParentProps<{ title: string; description?: string }>) {
  return <section class="hz-empty-state"><h2>{props.title}</h2><Show when={props.description}><p>{props.description}</p></Show>{props.children}</section>;
}
export function ErrorState(props: ParentProps<{ message: string }>) { return <section class="hz-error" role="alert"><p>{props.message}</p>{props.children}</section>; }
