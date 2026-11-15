import { Show, type ParentProps } from "solid-js";
import { Icon, type IconName } from "./icons";

export function Throbber(props: { size?: "small" | "medium" | "large"; label?: string }) {
  return <span class={`mc-throbber mc-throbber-${props.size ?? "medium"}`} role="status" aria-label={props.label ?? "Loading"} />;
}

export function ProgressBar(props: { value?: number; label?: string; detail?: string }) {
  return <div class="mc-progress" role="progressbar" aria-label={props.label} aria-valuenow={props.value === undefined ? undefined : Math.round(props.value * 100)}>
    <Show when={props.label || props.detail}><div class="mc-progress-text"><span>{props.label}</span><span>{props.detail}</span></div></Show>
    <div class="mc-progress-track" data-indeterminate={props.value === undefined || undefined}><i style={{ width: props.value === undefined ? undefined : `${Math.max(0, Math.min(100, props.value * 100))}%` }} /></div>
  </div>;
}

export function Toast(props: ParentProps<{ title: string; body?: string; icon?: IconName; image?: string; tone?: "info" | "error" | "success"; onClick?: () => void }>) {
  return <button type="button" tabIndex={-1} class="mc-toast" data-tone={props.tone ?? "info"} role={props.tone === "error" ? "alert" : "status"} onClick={() => props.onClick?.()}>
    <span class="mc-toast-icon"><Show when={props.image} fallback={<Icon name={props.icon ?? "notification"} size={28} />}>{image => <img src={image()} alt="" />}</Show></span>
    <span class="mc-toast-text"><strong>{props.title}</strong><Show when={props.body}><span>{props.body}</span></Show>{props.children}</span>
  </button>;
}

export function EmptyState(props: ParentProps<{ title: string; body?: string }>) {
  return <section class="mc-empty"><h2>{props.title}</h2><Show when={props.body}><p>{props.body}</p></Show>{props.children}</section>;
}
