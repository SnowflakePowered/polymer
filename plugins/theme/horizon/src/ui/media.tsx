import { For, Show, createEffect, createSignal, onCleanup, onMount, splitProps, type JSX, type ParentProps } from "solid-js";
import { Icon } from "./icons";
import type { Tone } from "./palette";
export { Icon, type IconName } from "./icons";
import a from "../assets/switch/button-a.svg";
import b from "../assets/switch/button-b.svg";
import l from "../assets/switch/button-l.svg";
import r from "../assets/switch/button-r.svg";
import plus from "../assets/switch/button-plus.svg";
import minus from "../assets/switch/button-minus.svg";
import pro from "../assets/switch/controller-pro.svg";
import console from "../assets/switch/controller-switch.svg";
import left from "../assets/switch/joycon-left-horizontal.svg";
import right from "../assets/switch/joycon-right-horizontal.svg";

const buttons = { a, b, l, r, plus, minus };
export type ButtonName = keyof typeof buttons | "x" | "y" | "home" | "dpad";
export function ButtonGlyph(props: { button: ButtonName }) {
  const source = () => buttons[props.button as keyof typeof buttons];
  return <Show when={source()} fallback={<kbd class="hz-button-glyph">{{ x: "X", y: "Y", home: "⌂", dpad: "✥" }[props.button as "x"]}</kbd>}><img class="hz-button-glyph" src={source()} alt={props.button.toUpperCase()} /></Show>;
}
export function ButtonChord(props: { buttons: readonly ButtonName[] }) {
  return <span class="hz-chord"><For each={props.buttons}>{(button, index) => <><Show when={index() > 0}> + </Show><ButtonGlyph button={button} /></>}</For></span>;
}
export function Avatar(props: { src?: string; label: string; tone?: Tone }) {
  return <span class="hz-avatar"><Show when={props.src} fallback={<Icon name="user" tone={props.tone} />}><img src={props.src} alt={props.label} /></Show></span>;
}
export function CoverArt(props: { src?: string; alt?: string; class?: string; onRatio?: (ratio: number) => void; onReady?: (ready: boolean) => void }) {
  const [failed, setFailed] = createSignal<string>();
  const source = () => props.src !== failed() ? props.src : undefined;
  return <Show when={source()}>{url => <img class={`hz-cover-art ${props.class ?? ""}`} src={url()} alt={props.alt ?? ""} loading="lazy" decoding="async"
    onLoad={event => { const image = event.currentTarget; if (!image.naturalHeight) return; props.onRatio?.(image.naturalWidth / image.naturalHeight); props.onReady?.(true); }}
    onError={() => { setFailed(url()); props.onReady?.(false); }} />}</Show>;
}
export function Tile(props: JSX.ButtonHTMLAttributes<HTMLButtonElement> & { selected?: boolean }) {
  const [local, rest] = splitProps(props, ["selected", "class"]);
  return <button type="button" {...rest} class={`hz-tile ${local.class ?? ""}`} data-selected={local.selected || undefined} aria-pressed={local.selected} />;
}
export function EmptySlot(props: { label: string }) { return <div class="hz-empty-slot">{props.label}</div>; }
export function PlayerBadge(props: { player: number }) { return <span class="hz-player-badge" data-player={props.player}>P{props.player + 1}</span>; }
const controllers = { pro, console, left, right };
export function ControllerIllustration(props: { kind: keyof typeof controllers; label?: string }) {
  return <img class="hz-controller-illustration" src={controllers[props.kind]} alt={props.label ?? props.kind} />;
}
export function ControllerSlot(props: ParentProps<{ player: number; name?: string; connected?: boolean }>) {
  return <section class="hz-controller-slot" data-connected={props.connected || undefined}><PlayerBadge player={props.player} /><p>{props.name ?? "Not assigned"}</p>{props.children}</section>;
}
export function ScrollingText(props: { children: string; active?: boolean }) {
  let root!: HTMLSpanElement, text!: HTMLSpanElement;
  const [overflow, setOverflow] = createSignal(0);
  const measure = () => setOverflow(Math.max(0, text.scrollWidth - root.clientWidth));
  createEffect(() => { props.children; let active = true; onCleanup(() => { active = false; }); queueMicrotask(() => { if (active) measure(); }); });
  onMount(() => { const observer = new ResizeObserver(measure); observer.observe(root); onCleanup(() => observer.disconnect()); });
  return <span ref={root} class="hz-scrolling-text" title={props.children} data-active={props.active && overflow() > 0 || undefined} style={{ "--hz-overflow": `${-overflow()}px` }}><span ref={text}>{props.children}</span></span>;
}
