import { For, Show, type JSX, type ParentProps } from "solid-js";
import { Icon, type IconName } from "./icons";
import { useNavigation, type ActionHint, type ActionName } from "./input";
import button_a from "../assets/prompts/pads/xbox_button_a.svg";
import button_b from "../assets/prompts/pads/xbox_button_b.svg";
import button_x from "../assets/prompts/pads/xbox_button_x.svg";
import button_y from "../assets/prompts/pads/xbox_button_y.svg";
import button_lb from "../assets/prompts/pads/xbox_lb.svg";
import button_rb from "../assets/prompts/pads/xbox_rb.svg";
import button_menu from "../assets/prompts/pads/xbox_button_menu.svg";
import button_dpad from "../assets/prompts/pads/xbox_dpad.svg";
import key_arrows from "../assets/prompts/keyboard/arrows.svg";
import key_enter from "../assets/prompts/keyboard/enter.svg";
import key_escape from "../assets/prompts/keyboard/escape.svg";
import key_shift from "../assets/prompts/keyboard/shift.svg";
import key_page_up from "../assets/prompts/keyboard/page-up.svg";
import key_page_down from "../assets/prompts/keyboard/page-down.svg";

export type GlyphName = "a" | "b" | "x" | "y" | "lb" | "rb" | "polymer" | "options" | "dpad";
const buttons: Record<Exclude<GlyphName, "polymer">, string> = { a: button_a, b: button_b, x: button_x, y: button_y, lb: button_lb, rb: button_rb, options: button_menu, dpad: button_dpad };
const keyboard: Record<string, string> = { arrows: key_arrows, enter: key_enter, escape: key_escape, shift: key_shift, "page-up": key_page_up, "page-down": key_page_down };

export function Glyph(props: { name: GlyphName }) {
  return <Show when={props.name !== "polymer"} fallback={<span class="mc-glyph" data-glyph="polymer">POLYMER</span>}><img class="mc-glyph" data-glyph={props.name} src={buttons[props.name as Exclude<GlyphName, "polymer">]} alt={props.name} /></Show>;
}

export function KeyCap(props: ParentProps) { return <kbd class="mc-keycap">{props.children}</kbd>; }

const GLYPHS: Record<ActionName, GlyphName> = { move: "dpad", activate: "a", back: "b", details: "x", pause: "polymer", previous: "lb", next: "rb" };
const KEYS: Record<ActionName, string> = { move: "arrows", activate: "enter", back: "escape", details: "shift", pause: "escape", previous: "page-up", next: "page-down" };

export function FooterLegend(props: { hints?: readonly ActionHint[]; menuLabel?: string; onMenu?: () => void }) {
  const navigation = useNavigation();
  const hints = () => (props.hints ?? navigation.hints()).filter(hint => hint.action !== "move");
  const controller = () => navigation.origin() === "controller";
  return <footer class="mc-footer">
    <Show when={props.menuLabel}><button type="button" tabIndex={-1} class="mc-footer-hint mc-footer-menu" onClick={() => props.onMenu?.()}><Glyph name="polymer" /><span>{props.menuLabel}</span></button></Show>
    <div class="mc-footer-spacer" />
    <For each={hints()}>{hint => <span class="mc-footer-hint"><Show when={controller()} fallback={<Show when={hint.keyboard} fallback={<img class="mc-glyph" data-glyph="key" src={keyboard[KEYS[hint.action]]} alt={KEYS[hint.action]} />}><KeyCap>{hint.keyboard}</KeyCap></Show>}><Glyph name={GLYPHS[hint.action]} /></Show><span>{hint.label}</span></span>}</For>
  </footer>;
}

export type ChromeAction = { icon: IconName; label: string; run(): void };

export type ChromeSearch = { label: string; value: string; onInput(text: string): void };

export function TopBar(props: ParentProps<{ title?: JSX.Element; search?: ChromeSearch; actions?: readonly ChromeAction[] }>) {
  return <header class="mc-topbar">
    <div class="mc-topbar-title">{props.title}</div>
    <Show when={props.search}>{search => <label class="mc-topbar-search"><Icon name="search" size={16} color="#c5c5c5" />
      <span class="mc-topbar-search-field"><Show when={!search().value}><span class="mc-topbar-search-label">{search().label}</span></Show>
        <input class="mc-topbar-search-input" type="search" aria-label={search().label} value={search().value} onInput={event => search().onInput(event.currentTarget.value)} /></span>
    </label>}</Show>
    <div class="mc-topbar-actions">
      {props.children}
      <For each={props.actions}>{action => <button type="button" tabIndex={-1} class="mc-topbar-icon" aria-label={action.label} title={action.label} onClick={action.run}><Icon name={action.icon} size={20} /></button>}</For>
    </div>
  </header>;
}

export type RailItem = ChromeAction & { active?: boolean; badge?: boolean };

export function NavRail(props: { items: readonly RailItem[]; footer?: readonly RailItem[] }) {
  const item = (entry: RailItem) => <button type="button" tabIndex={-1} class="mc-rail-item" data-active={entry.active || undefined} aria-label={entry.label} title={entry.label} onClick={entry.run}>
    <Icon name={entry.icon} filled={entry.active} size={24} color={entry.active ? "#ffffff" : "#d0d0d0"} />
    <Show when={entry.badge}><span class="mc-rail-badge" /></Show>
  </button>;
  return <nav class="mc-rail" aria-label="Navigation">
    <span class="mc-rail-mark" aria-hidden="true">P</span>
    <For each={props.items}>{item}</For>
    <div class="mc-rail-spacer" />
    <For each={props.footer}>{item}</For>
  </nav>;
}
