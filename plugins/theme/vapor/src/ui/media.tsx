import { For, Show, createSignal, splitProps, type JSX, type ParentProps } from "solid-js";
import { Icon } from "./icons";

export function Artwork(props: { src?: string; alt?: string; class?: string; fallback?: JSX.Element; onRatio?: (ratio: number) => void }) {
  const [failed, setFailed] = createSignal<string>();
  return <Show when={props.src && props.src !== failed() ? props.src : undefined} fallback={props.fallback}>{src =>
    <img class={props.class} src={src()} alt={props.alt ?? ""} decoding="async" onError={() => setFailed(src())}
      onLoad={event => { const image = event.currentTarget; if (image.naturalHeight) props.onRatio?.(image.naturalWidth / image.naturalHeight); }} />
  }</Show>;
}

export type CapsuleProps = JSX.ButtonHTMLAttributes<HTMLButtonElement> & { title: string; subtitle?: string; art?: string; logo?: string; ratio?: number; badge?: string; progress?: number; wide?: boolean };

/** A library capsule sized to its cover art that grows and gains a light frame on focus. */
export function Capsule(props: CapsuleProps) {
  const [local, rest] = splitProps(props, ["title", "subtitle", "art", "logo", "ratio", "badge", "progress", "wide", "class", "style"]);
  const [measured, setMeasured] = createSignal<number>();
  const ratio = () => local.wide ? undefined : local.ratio ?? measured();
  return <button type="button" {...rest} class={`vp-capsule ${local.class ?? ""}`} data-wide={local.wide || undefined} aria-label={local.subtitle ? `${local.title}, ${local.subtitle}` : local.title}
    style={ratio() ? { width: `${Math.round(258 * Math.min(2.4, Math.max(.5, ratio()!)))}px` } : undefined}>
    <span class="vp-capsule-art">
      <Artwork src={local.art} onRatio={setMeasured} fallback={<span class="vp-capsule-fallback"><span>{local.title}</span></span>} />
      <Show when={local.logo}>{logo => <span class="vp-capsule-logo"><img src={logo()} alt="" /></span>}</Show>
      <Show when={local.badge}><span class="vp-capsule-badge">{local.badge}</span></Show>
      <Show when={local.progress !== undefined}><span class="vp-capsule-progress"><i style={{ width: `${Math.round((local.progress ?? 0) * 100)}%` }} /></span></Show>
    </span>
  </button>;
}

export function Shelf(props: ParentProps<{ title?: string; class?: string }>) {
  return <section class={`vp-shelf ${props.class ?? ""}`}><Show when={props.title}><h2 class="vp-shelf-title">{props.title}</h2></Show><div class="vp-shelf-row">{props.children}</div></section>;
}

export function CapsuleGrid(props: ParentProps<{ class?: string }>) {
  return <div class={`vp-grid ${props.class ?? ""}`}>{props.children}</div>;
}

/** Game details header: blurred backdrop, hero art and a logo or title over a gradient. */
export function Hero(props: ParentProps<{ title: string; art?: string; logo?: string }>) {
  return <header class="vp-hero">
    <Artwork class="vp-hero-art" src={props.art} fallback={<span class="vp-hero-art vp-hero-fallback" />} />
    <span class="vp-hero-shade" />
    <div class="vp-hero-title"><Artwork class="vp-hero-logo" src={props.logo} alt={props.title} fallback={<h1>{props.title}</h1>} /></div>
    {props.children}
  </header>;
}

export function PlayBar(props: ParentProps<{ action: JSX.Element; stats?: readonly { label: string; value: string }[] }>) {
  return <div class="vp-playbar">
    {props.action}
    <For each={props.stats}>{stat => <div class="vp-playbar-stat"><small>{stat.label}</small><span>{stat.value}</span></div>}</For>
    <div class="vp-playbar-spacer" />
    {props.children}
  </div>;
}

export function Avatar(props: { src?: string; label: string }) {
  return <span class="vp-avatar" title={props.label}><Artwork src={props.src} alt={props.label} fallback={<Icon name="user" color="#8b929a" />} /></span>;
}
