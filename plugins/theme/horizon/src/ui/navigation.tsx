import { Tabs as ArkTabs, Listbox, createListCollection, useTabs, useListbox } from "@ark-ui/solid";
import { For, Index, Show, createMemo, createSignal, type JSX, type ParentProps } from "solid-js";
import { useFocusRing, useInputScope, useNavigation } from "./input";
import { ScrollingText } from "./media";

export type NavigationItem = { value: string; label: string; description?: string; disabled?: boolean };
export function Tabs(props: { value: string; onValueChange(value: string): void; items: readonly (NavigationItem & { content: JSX.Element })[] }) {
  let root!: HTMLDivElement;
  const api = useTabs(() => ({ value: props.value, onValueChange: details => props.onValueChange(details.value) }));
  useInputScope(() => ({ root, handle: action => {
    const arrow = action.origin === "controller" && action.type === "move" && !!document.activeElement?.closest("[role=tablist]");
    if (action.type !== "previous" && action.type !== "next" && !(arrow && (action.direction === "left" || action.direction === "right"))) return false;
    const items = props.items.filter(item => !item.disabled), index = items.findIndex(item => item.value === props.value);
    const next = items[(index + (action.type === "previous" || action.direction === "left" ? -1 : 1) + items.length) % items.length];
    if (next) { api().setValue(next.value); if (arrow) root.querySelector<HTMLElement>(`[data-part="trigger"][data-value="${CSS.escape(next.value)}"]`)?.focus(); }
    return true;
  } }));
  return <ArkTabs.RootProvider ref={root} value={api} class="hz-tabs"><ArkTabs.List><For each={props.items}>{item => <ArkTabs.Trigger value={item.value} disabled={item.disabled}>{item.label}</ArkTabs.Trigger>}</For></ArkTabs.List><For each={props.items}>{item => <ArkTabs.Content value={item.value}>{item.content}</ArkTabs.Content>}</For></ArkTabs.RootProvider>;
}
export function CategoryNavigation(props: { label: string; value: string; items: readonly NavigationItem[]; onValueChange(value: string): void; onEnter?: () => void }) {
  let root!: HTMLElement;
  const navigation = useNavigation();
  useInputScope(() => ({ root, navigation: true, handle: action => {
    if (props.onEnter && (action.type === "activate" || action.type === "move" && action.direction === "right")) { props.onEnter(); return true; }
    const step = action.type === "previous" ? -5 : action.type === "next" ? 5 : action.type === "move" && action.direction === "up" ? -1 : action.type === "move" && action.direction === "down" ? 1 : 0;
    if (!step) return false;
    const items = navigation.items(root), index = items.indexOf(document.activeElement as HTMLElement);
    const item = items[Math.max(0, Math.min(items.length - 1, index + step))];
    item?.focus(); item?.scrollIntoView?.({ block: "nearest" });
    return true;
  } }));
  return <nav ref={root} class="hz-categories" aria-label={props.label}><Index each={props.items}>{item => <button type="button" disabled={item().disabled} aria-current={props.value === item().value ? "true" : undefined} onFocus={() => props.onValueChange(item().value)} onClick={() => props.onValueChange(item().value)}>{item().label}</button>}</Index></nav>;
}
export function ListRow(props: ParentProps<{ label: string; description?: string; focused?: boolean }>) {
  return <div class="hz-list-row"><span><ScrollingText active={props.focused}>{props.label}</ScrollingText><Show when={props.description}><small>{props.description}</small></Show></span>{props.children}</div>;
}
export function SelectableList(props: { label: string; value: string | null; items: readonly NavigationItem[]; onValueChange(value: string): void; onActivate?: (value: string) => void }) {
  let root!: HTMLDivElement;
  const [focused, setFocused] = createSignal(false);
  const collection = createMemo(() => createListCollection({ items: [...props.items] }));
  useFocusRing(() => root, () => { props.value; return root?.querySelector("[data-part='item'][data-state='checked']"); });
  const api = useListbox(() => ({ scrollToIndexFn: details => details.getElement()?.scrollIntoView?.({ block: "nearest" }), collection: collection(), value: props.value ? [props.value] : [], selectionMode: "single", selectOnHighlight: true, onValueChange: details => { if (details.value[0]) props.onValueChange(details.value[0]); } }));
  useInputScope(() => ({ root, handle: action => {
    if (action.origin !== "controller") return false;
    if (action.type === "activate" && props.value) { props.onActivate?.(props.value); return true; }
    const step = action.type === "previous" ? -5 : action.type === "next" ? 5 : action.direction === "up" ? -1 : action.direction === "down" ? 1 : 0;
    if (!step) return false;
    const items = props.items.filter(item => !item.disabled), index = items.findIndex(item => item.value === props.value);
    const next = items[Math.max(0, Math.min(items.length - 1, index + step))];
    if (next) { api().selectValue(next.value); root.querySelector<HTMLElement>(`[data-value='${CSS.escape(next.value)}']`)?.scrollIntoView?.({ block: "nearest" }); }
    return true;
  } }));
  return <Listbox.RootProvider onFocusIn={() => setFocused(true)} onFocusOut={event => { if (!root.contains(event.relatedTarget as Node)) setFocused(false); }} ref={root} value={api} class="hz-list"><Listbox.Label class="hz-visually-hidden">{props.label}</Listbox.Label><Listbox.Content onKeyDown={event => { if (event.key === "Enter" && props.value) props.onActivate?.(props.value); }}><For each={props.items}>{item => <Listbox.Item item={item}><Listbox.ItemText><ListRow label={item.label} description={item.description} focused={focused() && props.value === item.value} /></Listbox.ItemText></Listbox.Item>}</For></Listbox.Content></Listbox.RootProvider>;
}
export function TileGrid(props: ParentProps<JSX.HTMLAttributes<HTMLDivElement>>) { return <div {...props} class={`hz-tile-grid ${props.class ?? ""}`} />; }
