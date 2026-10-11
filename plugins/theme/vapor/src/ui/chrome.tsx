import { For, Show, createSignal, onCleanup, onMount, type JSX, type ParentProps } from "solid-js";
import { Icon, type IconName } from "./icons";
import { useNavigation, type ActionHint, type ActionName } from "./input";

export type GlyphName = "a" | "b" | "x" | "y" | "lb" | "rb" | "polymer" | "options" | "dpad";
const LETTERS: Partial<Record<GlyphName, string>> = { a: "A", b: "B", x: "X", y: "Y", lb: "L1", rb: "R1" };

export function Glyph(props: { name: GlyphName }) {
  return <Show when={props.name !== "polymer" && props.name !== "options"} fallback={
    <span class="vp-glyph" data-glyph={props.name}>{props.name === "polymer" ? "POLYMER" : <svg width="22" height="14" viewBox="0 0 22 14" aria-hidden="true"><rect x="0" y="0" width="22" height="14" rx="7" fill="#ffffff" /><path d="M6 5h10M6 9h10" stroke="#0e141b" stroke-width="1.6" /></svg>}</span>
  }><span class="vp-glyph" data-glyph={props.name}>{props.name === "dpad" ? "✥" : LETTERS[props.name]}</span></Show>;
}

export function KeyCap(props: ParentProps) { return <kbd class="vp-keycap">{props.children}</kbd>; }

const GLYPHS: Record<ActionName, GlyphName> = { move: "dpad", activate: "a", back: "b", details: "x", pause: "polymer", previous: "lb", next: "rb" };
const KEYS: Record<ActionName, string> = { move: "Arrows", activate: "Enter", back: "Esc", details: "Shift", pause: "Esc", previous: "PgUp", next: "PgDn" };

export function FooterLegend(props: { hints?: readonly ActionHint[]; menuLabel?: string; onMenu?: () => void }) {
  const navigation = useNavigation();
  const hints = () => (props.hints ?? navigation.hints()).filter(hint => hint.action !== "move");
  const controller = () => navigation.origin() === "controller";
  return <footer class="vp-footer">
    <Show when={props.menuLabel}><button type="button" tabIndex={-1} class="vp-footer-hint vp-footer-menu" onClick={() => props.onMenu?.()}><Glyph name="polymer" /><span>{props.menuLabel}</span></button></Show>
    <div class="vp-footer-spacer" />
    <For each={hints()}>{hint => <span class="vp-footer-hint"><Show when={controller()} fallback={<KeyCap>{hint.keyboard ?? KEYS[hint.action]}</KeyCap>}><Glyph name={GLYPHS[hint.action]} /></Show><span>{hint.label}</span></span>}</For>
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
