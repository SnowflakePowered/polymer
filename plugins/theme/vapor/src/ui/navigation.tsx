import { For, Index, Show, createEffect, createMemo, type JSX, type ParentProps } from "solid-js";
import { Glyph } from "./chrome";
import { Icon, useFocused, type IconName } from "./icons";
import { useInputScope, useNavigation, type ActionHint, type InputAction } from "./input";

export function Page(props: ParentProps<{ class?: string; hints?: readonly ActionHint[]; focusWhen?: () => unknown; onAction?: (action: InputAction) => boolean }>) {
  let root!: HTMLElement;
  const navigation = useNavigation();
  useInputScope(() => ({ root, navigation: true, hints: () => props.hints ?? [], handle: action => props.onAction?.(action) ?? false }));
  const overlay = createMemo(() => navigation.hasOverlay());
  createEffect(() => {
    props.focusWhen?.();
    if (overlay()) return;
    requestAnimationFrame(() => {
      if (root.isConnected && !navigation.overlayOpen() && !root.contains(document.activeElement)) (root.querySelector<HTMLElement>("[data-autofocus]") ?? navigation.items(root)[0])?.focus();
    });
  });
  return <main ref={root} class={`vp-page ${props.class ?? ""}`}>{props.children}</main>;
}

export type Tab = { id: string; title: string; badge?: JSX.Element; disabled?: boolean };

export function Tabs(props: { tabs: readonly Tab[]; active: string; onChange(id: string): void; glyphs?: boolean }) {
  let root!: HTMLDivElement;
  useInputScope(() => ({ root: root.closest<HTMLElement>(".vp-page") ?? root, handle: action => {
    if (action.type !== "previous" && action.type !== "next") return false;
    const tabs = props.tabs.filter(tab => !tab.disabled), index = tabs.findIndex(tab => tab.id === props.active);
    const next = tabs[(index + (action.type === "previous" ? -1 : 1) + tabs.length) % tabs.length];
    if (next) props.onChange(next.id);
    return true;
  } }));
  return <div ref={root} class="vp-tabs">
    <Show when={props.glyphs !== false}><Glyph name="lb" /></Show>
    <For each={props.tabs}>{tab => <button type="button" class="vp-tab" aria-pressed={tab.id === props.active} disabled={tab.disabled} onClick={() => props.onChange(tab.id)}><span>{tab.title}</span><Show when={tab.badge}><span class="vp-tab-badge">{tab.badge}</span></Show></button>}</For>
    <Show when={props.glyphs !== false}><Glyph name="rb" /></Show>
  </div>;
}

export type SettingsPage = { id: string; title: string; icon?: IconName; separatorBefore?: boolean; content: () => JSX.Element };

/** PagedSettings: a left category list whose focus selects the page shown on the right. */
export function PagedSettings(props: { title?: string; pages: readonly SettingsPage[]; active: string; onChange(id: string): void }) {
  let list!: HTMLElement, content!: HTMLElement;
  const navigation = useNavigation();
  const active = () => props.pages.find(page => page.id === props.active) ?? props.pages[0];
  useInputScope(() => ({ root: list, navigation: true, handle: action => {
    if (action.type === "activate" || action.type === "move" && action.direction === "right") { navigation.items(content)[0]?.focus(); return true; }
    if (action.type !== "move" || (action.direction !== "up" && action.direction !== "down")) return false;
    const items = navigation.items(list), index = items.indexOf(document.activeElement as HTMLElement);
    const next = items[Math.max(0, Math.min(items.length - 1, index + (action.direction === "up" ? -1 : 1)))];
    next?.focus(); next?.scrollIntoView?.({ block: "nearest" });
    return true;
  } }));
  useInputScope(() => ({ root: content, navigation: true, handle: action => {
    if (action.type !== "back" && !(action.type === "move" && action.direction === "left")) return false;
    if (action.type === "move" && navigation.move(content, "left")) return true;
    list.querySelector<HTMLElement>("[aria-current='page']")?.focus();
    return true;
  } }));
  return <div class="vp-paged">
    <nav ref={list} class="vp-paged-list" aria-label={props.title ?? "Settings"}>
      <Show when={props.title}><h1 class="vp-paged-heading">{props.title}</h1></Show>
      <Index each={props.pages}>{page => <>
        <Show when={page().separatorBefore}><hr class="vp-paged-separator" /></Show>
        <button type="button" class="vp-paged-item" aria-current={page().id === active()?.id ? "page" : undefined} data-autofocus={page().id === active()?.id || undefined} onFocus={() => props.onChange(page().id)} onClick={() => props.onChange(page().id)}>
          <span class="vp-paged-item-inner"><Show when={page().icon}>{icon => <Icon name={icon()} size={20} color={page().id === active()?.id ? "#ffffff" : "#b8bcbf"} />}</Show><span>{page().title}</span></span>
        </button>
      </>}</Index>
    </nav>
    <section ref={content} class="vp-paged-content"><h2 class="vp-paged-title">{active()?.title}</h2>{active()?.content()}</section>
  </div>;
}

export function PanelSection(props: ParentProps<{ title?: string }>) {
  return <section class="vp-panel-section"><Show when={props.title}><h3 class="vp-panel-section-title">{props.title}</h3></Show>{props.children}</section>;
}

export function MenuItem(props: { icon?: IconName; label: string; active?: boolean; onClick(): void; "data-autofocus"?: boolean }) {
  const [focused, focus] = useFocused();
  return <button type="button" class="vp-menu-entry" {...focus} data-active={props.active || undefined} data-autofocus={props["data-autofocus"]} onClick={props.onClick}>
    <Show when={props.icon}>{icon => <Icon name={icon()} size={20} color={focused() || props.active ? "#ffffff" : "#b8bcbf"} />}</Show><span>{props.label}</span>
  </button>;
}
