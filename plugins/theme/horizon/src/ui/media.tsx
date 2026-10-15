import { For, Show, createEffect, createSignal, onCleanup, onMount, splitProps, type JSX, type ParentProps } from "solid-js";
import { Icon } from "./icons";
import type { Tone } from "./palette";
export { Icon, type IconName } from "./icons";

/** A data URL image. Console banner formats a browser cannot display are drawn
 * by the shell's native banner renderer. */
export function BannerImage(props: { src: string; class?: string }) {
  return <img class={props.class} src={props.src} alt="" />;
}
import pro from "../assets/switch/controller-pro.svg";
import console from "../assets/switch/controller-switch.svg";
import left from "../assets/switch/joycon-left-horizontal.svg";
import right from "../assets/switch/joycon-right-horizontal.svg";

import button_a from "../assets/prompts/switch/a.svg";
import button_b from "../assets/prompts/switch/b.svg";
import button_x from "../assets/prompts/switch/x.svg";
import button_y from "../assets/prompts/switch/y.svg";
import button_l from "../assets/prompts/switch/l.svg";
import button_r from "../assets/prompts/switch/r.svg";
import button_plus from "../assets/prompts/switch/plus.svg";
import button_minus from "../assets/prompts/switch/minus.svg";
import button_home from "../assets/prompts/switch/home.svg";
import button_dpad from "../assets/prompts/switch/dpad.svg";
import key_arrows from "../assets/prompts/keyboard/arrows.svg";
import key_enter from "../assets/prompts/keyboard/enter.svg";
import key_escape from "../assets/prompts/keyboard/escape.svg";
import key_shift from "../assets/prompts/keyboard/shift.svg";
import key_page_up from "../assets/prompts/keyboard/page-up.svg";
import key_page_down from "../assets/prompts/keyboard/page-down.svg";
import key_tab from "../assets/prompts/keyboard/tab.svg";
import key_space from "../assets/prompts/keyboard/space.svg";

const buttons = { a: button_a, b: button_b, x: button_x, y: button_y, l: button_l, r: button_r, plus: button_plus, minus: button_minus, home: button_home, dpad: button_dpad };
const keyboard = { arrows: key_arrows, enter: key_enter, escape: key_escape, shift: key_shift, "page-up": key_page_up, "page-down": key_page_down, tab: key_tab, space: key_space };
export type ButtonName = "a" | "b" | "x" | "y" | "l" | "r" | "plus" | "minus" | "home" | "dpad";
export type KeyName = "arrows" | "enter" | "escape" | "shift" | "page-up" | "page-down" | "tab" | "space";
export function ButtonGlyph(props: { button: ButtonName }) {
  return <img class="hz-button-glyph" src={buttons[props.button]} alt={props.button.toUpperCase()} />;
}
export function KeyGlyph(props: { name: KeyName }) {
  return <img class="hz-button-glyph" src={keyboard[props.name]} alt={props.name} />;
}
export function ButtonChord(props: { buttons: readonly ButtonName[] }) {
  return <span class="hz-chord"><For each={props.buttons}>{(button, index) => <><Show when={index() > 0}> + </Show><ButtonGlyph button={button} /></>}</For></span>;
}
export function Avatar(props: { src?: string; label: string; tone?: Tone }) {
  return <span class="hz-avatar"><Show when={props.src} fallback={<Icon name="user" tone={props.tone} />}><img src={props.src} alt={props.label} /></Show></span>;
}
export function CoverArt(props: { src?: string; blurhash?: string; alt?: string; class?: string; onRatio?: (ratio: number) => void; onReady?: (ready: boolean) => void }) {
  const [failed, setFailed] = createSignal<string>();
  const source = () => props.src !== failed() ? props.src : undefined;
  return <Show when={source()}>{url => <img class={`hz-cover-art ${props.class ?? ""}`} data-blurhash={props.blurhash} src={url()} alt={props.alt ?? ""} loading="lazy" decoding="async"
    onLoad={event => { const image = event.currentTarget; if (!image.naturalHeight) return; props.onRatio?.(image.naturalWidth / image.naturalHeight); props.onReady?.(true); }}
    onError={() => { setFailed(url()); props.onReady?.(false); }} />}</Show>;
}
export function Tile(props: JSX.ButtonHTMLAttributes<HTMLButtonElement> & { selected?: boolean }) {
  const [local, rest] = splitProps(props, ["selected", "class", "classList"]);
  return <button type="button" {...rest} class={`hz-tile ${local.class ?? ""}`} classList={local.classList} data-selected={local.selected || undefined} aria-pressed={local.selected} />;
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
