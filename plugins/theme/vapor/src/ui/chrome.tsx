import { For, Show, createSignal, onCleanup, onMount, type JSX, type ParentProps } from "solid-js";
import { Icon, type IconName } from "./icons";
import { useNavigation, type ActionHint, type ActionName } from "./input";
import button_a from "../assets/prompts/steamdeck/a.svg";
import button_b from "../assets/prompts/steamdeck/b.svg";
import button_x from "../assets/prompts/steamdeck/x.svg";
import button_y from "../assets/prompts/steamdeck/y.svg";
import button_l1 from "../assets/prompts/steamdeck/l1.svg";
import button_r1 from "../assets/prompts/steamdeck/r1.svg";
import button_options from "../assets/prompts/steamdeck/options.svg";
import button_dpad from "../assets/prompts/steamdeck/dpad.svg";
import key_arrows from "../assets/prompts/keyboard/arrows.svg";
import key_enter from "../assets/prompts/keyboard/enter.svg";
import key_escape from "../assets/prompts/keyboard/escape.svg";
import key_shift from "../assets/prompts/keyboard/shift.svg";
import key_page_up from "../assets/prompts/keyboard/page-up.svg";
import key_page_down from "../assets/prompts/keyboard/page-down.svg";

export type GlyphName = "a" | "b" | "x" | "y" | "lb" | "rb" | "polymer" | "options" | "dpad";
const buttons: Record<string, string> = { a: button_a, b: button_b, x: button_x, y: button_y, l1: button_l1, r1: button_r1, options: button_options, dpad: button_dpad };
const keyboard: Record<string, string> = { arrows: key_arrows, enter: key_enter, escape: key_escape, shift: key_shift, "page-up": key_page_up, "page-down": key_page_down };
const SOURCES: Record<Exclude<GlyphName, "polymer">, string> = { a: "a", b: "b", x: "x", y: "y", lb: "l1", rb: "r1", options: "options", dpad: "dpad" };

export function Glyph(props: { name: GlyphName }) {
  return <Show when={props.name !== "polymer"} fallback={<span class="vp-glyph" data-glyph="polymer">POLYMER</span>}><img class="vp-glyph" data-glyph={props.name} src={buttons[SOURCES[props.name as Exclude<GlyphName, "polymer">]]} alt={props.name} /></Show>;
}

export function KeyCap(props: ParentProps) { return <kbd class="vp-keycap">{props.children}</kbd>; }

const GLYPHS: Record<ActionName, GlyphName> = { move: "dpad", activate: "a", back: "b", details: "x", pause: "polymer", previous: "lb", next: "rb" };
const KEYS: Record<ActionName, string> = { move: "arrows", activate: "enter", back: "escape", details: "shift", pause: "escape", previous: "page-up", next: "page-down" };

export function FooterLegend(props: { hints?: readonly ActionHint[]; menuLabel?: string; onMenu?: () => void }) {
  const navigation = useNavigation();
  const hints = () => (props.hints ?? navigation.hints()).filter(hint => hint.action !== "move");
  const controller = () => navigation.origin() === "controller";
  return <footer class="vp-footer">
    <Show when={props.menuLabel}><button type="button" tabIndex={-1} class="vp-footer-hint vp-footer-menu" onClick={() => props.onMenu?.()}><Glyph name="polymer" /><span>{props.menuLabel}</span></button></Show>
    <div class="vp-footer-spacer" />
    <For each={hints()}>{hint => <span class="vp-footer-hint"><Show when={controller()} fallback={<Show when={hint.keyboard} fallback={<img class="vp-glyph" data-glyph="key" src={keyboard[KEYS[hint.action]]} alt={KEYS[hint.action]} />}><KeyCap>{hint.keyboard}</KeyCap></Show>}><Glyph name={GLYPHS[hint.action]} /></Show><span>{hint.label}</span></span>}</For>
  </footer>;
}

export function Clock() {
  const format = () => new Date().toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  const [time, setTime] = createSignal(format());
  onMount(() => { const timer = setInterval(() => setTime(format()), 10_000); onCleanup(() => clearInterval(timer)); });
  return <span class="vp-clock">{time()}</span>;
}

export function TopBar(props: { title?: JSX.Element; actions?: readonly { icon: IconName; label: string; run(): void }[] }) {
  return <header class="vp-topbar">
    <div class="vp-topbar-title">{props.title}</div>
    <For each={props.actions}>{action => <button type="button" tabIndex={-1} class="vp-topbar-icon" aria-label={action.label} title={action.label} onClick={action.run}><Icon name={action.icon} size={26} /></button>}</For>
    <Clock />
  </header>;
}
