import { splitProps, Show, type JSX, type ParentProps } from "solid-js";
import { Icon, type IconName } from "./media";
import type { Tone } from "./palette";

export type ButtonProps = JSX.ButtonHTMLAttributes<HTMLButtonElement> & { appearance?: "primary" | "secondary" | "danger"; pending?: boolean };
export function Button(props: ButtonProps) {
  const [local, rest] = splitProps(props, ["appearance", "pending", "class", "children", "disabled"]);
  return <button type="button" {...rest} class={`hz-button ${local.class ?? ""}`} data-appearance={local.appearance ?? "secondary"} aria-busy={local.pending || undefined} disabled={local.disabled || local.pending}>
    <Show when={local.pending}><span class="hz-spinner" aria-hidden="true" /></Show>{local.children}
  </button>;
}
export function IconButton(props: ButtonProps & { icon?: IconName; label: string; tone?: Tone }) {
  const [local, rest] = splitProps(props, ["icon", "label", "tone", "children"]);
  return <Button {...rest} aria-label={local.label} title={local.label}><Show when={local.icon}>{icon => <Icon name={icon()} tone={local.tone ?? (rest.appearance === "primary" ? "onAction" : rest.appearance === "danger" ? "danger" : "action")} />}</Show>{local.children}</Button>;
}
export function ActionRow(props: ButtonProps & { description?: string }) {
  const [local, rest] = splitProps(props, ["description", "children", "class"]);
  return <Button {...rest} class={`hz-action-row ${local.class ?? ""}`}><span>{local.children}<Show when={local.description}><small>{local.description}</small></Show></span></Button>;
}
export function ActionGroup(props: ParentProps) { return <div class="hz-actions">{props.children}</div>; }
