import { For, Show, createSignal, splitProps, type JSX, type ParentProps } from "solid-js";
import { Icon } from "./icons";
import { Throbber } from "./feedback";

export function Artwork(props: { src?: string; alt?: string; class?: string; fallback?: JSX.Element }) {
  const [failed, setFailed] = createSignal<string>();
  return <Show when={props.src && props.src !== failed() ? props.src : undefined} fallback={props.fallback}>{src =>
    <img class={props.class} src={src()} alt={props.alt ?? ""} decoding="async" onError={() => setFailed(src())} />
  }</Show>;
}

export type CapsuleProps = JSX.ButtonHTMLAttributes<HTMLButtonElement> & { title: string; subtitle?: string; art?: string; logo?: string; badge?: string; progress?: number; wide?: boolean; compact?: boolean; busy?: boolean };

export function Capsule(props: CapsuleProps) {
  const [local, rest] = splitProps(props, ["title", "subtitle", "art", "logo", "badge", "progress", "wide", "compact", "busy", "class"]);
  return <button type="button" {...rest} class={`mc-capsule ${local.class ?? ""}`} data-wide={local.wide || undefined} data-compact={local.compact || undefined} aria-busy={local.busy || undefined} aria-label={local.subtitle ? `${local.title}, ${local.subtitle}` : local.title}>
    <span class="mc-capsule-art">
      <Artwork src={local.art} fallback={<span class="mc-capsule-fallback"><Icon name="controller" size={40} color="#9aa0a8" /><Show when={local.compact}><span>{local.title}</span></Show></span>} />
      <Show when={local.logo}>{logo => <span class="mc-capsule-logo"><img src={logo()} alt="" /></span>}</Show>
      <Show when={local.busy}><span class="mc-capsule-busy"><Throbber label={`Closing ${local.title}`} /></span></Show>
      <Show when={local.progress !== undefined}><span class="mc-capsule-progress"><i style={{ width: `${Math.round((local.progress ?? 0) * 100)}%` }} /></span></Show>
      <Show when={local.compact || local.wide}><span class="mc-capsule-caption"><strong>{local.title}</strong><Show when={local.subtitle}><span>{local.subtitle}</span></Show></span></Show>
    </span>
    <Show when={!local.compact && !local.wide}><span class="mc-capsule-info">
      <strong>{local.title}</strong>
      <Show when={local.subtitle}><span>{local.subtitle}</span></Show>
      <Show when={local.badge}><span class="mc-capsule-status" data-warning><Icon name="error" size={16} color="#fce100" />{local.badge}</span></Show>
    </span></Show>
  </button>;
}

export function Shelf(props: ParentProps<{ title?: string; class?: string }>) {
  return <section class={`mc-shelf ${props.class ?? ""}`}><Show when={props.title}><h2 class="mc-shelf-title">{props.title}</h2></Show><div class="mc-shelf-row">{props.children}</div></section>;
}

export function CapsuleGrid(props: ParentProps<{ class?: string }>) {
  return <div class={`mc-grid ${props.class ?? ""}`}>{props.children}</div>;
}

export function Hero(props: ParentProps<{ title: string; subtitle?: string; art?: string; logo?: string; banner?: string; cover?: string }>) {
  return <header class="mc-hero">
    <span class="mc-hero-backdrop"><Artwork class="mc-hero-art" src={props.art ?? props.cover} /></span>
    <div class="mc-hero-heading">
      <Show when={props.banner ?? props.cover}>{image => <img class="mc-hero-cover" data-banner={props.banner ? "" : undefined} src={image()} alt="" />}</Show>
      <div class="mc-hero-text">
        <Artwork class="mc-hero-logo" src={props.logo} alt={props.title} fallback={<h1>{props.title}</h1>} />
        <Show when={props.subtitle}><span>{props.subtitle}</span></Show>
      </div>
    </div>
    {props.children}
  </header>;
}

export function PlayBar(props: ParentProps<{ action: JSX.Element; stats?: readonly { label: string; value: string }[] }>) {
  return <div class="mc-playbar">
    {props.action}
    <For each={props.stats}>{stat => <div class="mc-playbar-stat"><small>{stat.label}</small><span>{stat.value}</span></div>}</For>
    <div class="mc-playbar-spacer" />
    {props.children}
  </div>;
}

export function Avatar(props: { src?: string; label: string }) {
  return <span class="mc-avatar" title={props.label}><Artwork src={props.src} alt={props.label} fallback={<Icon name="user" color="#9aa0a8" />} /></span>;
}
