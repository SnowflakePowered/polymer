import { Show, splitProps, type JSX } from "solid-js";
import { Icon, type IconName } from "./icons";

export type ButtonProps = JSX.ButtonHTMLAttributes<HTMLButtonElement> & { primary?: boolean; danger?: boolean; "data-autofocus"?: boolean };

export function Button(props: ButtonProps) {
  const [local, rest] = splitProps(props, ["primary", "danger", "class", "children"]);
  return <button type="button" {...rest} class={`mc-button ${local.class ?? ""}`} data-primary={local.primary || undefined} data-danger={local.danger || undefined}><span>{local.children}</span></button>;
}

export function IconButton(props: ButtonProps & { icon: IconName; label: string }) {
  const [local, rest] = splitProps(props, ["icon", "label", "class"]);
  return <button type="button" {...rest} class={`mc-icon-button ${local.class ?? ""}`} aria-label={local.label} title={local.label}><Icon name={local.icon} color="#ffffff" /></button>;
}

export function PlayButton(props: ButtonProps & { state?: "play" | "launching" | "running" }) {
  const [local, rest] = splitProps(props, ["state", "class"]);
  return <button type="button" {...rest} class={`mc-play-button ${local.class ?? ""}`} data-state={local.state ?? "play"} disabled={local.state === "launching" || rest.disabled}>
    <Show when={local.state !== "launching"} fallback={<span class="mc-throbber mc-throbber-small" aria-hidden="true" />}><Icon name={local.state === "running" ? "stop" : "play"} /></Show>
    <span>{local.state === "launching" ? "Launching" : local.state === "running" ? "Stop" : "Play"}</span>
  </button>;
}
