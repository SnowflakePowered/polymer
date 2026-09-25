import { Select as Ark, createListCollection, useSelect } from "@ark-ui/solid";
import { For, Show, createEffect, createMemo, onCleanup, onMount } from "solid-js";
import { Button } from "../actions";
import { Field, fieldAccessibility } from "../forms";
import { useFocusRing, useInputScope, useNavigation, type Direction } from "../input";
import { OverlayPortal } from "../portal";
import { ChoiceSession, type ChoiceItem, type ChoiceProps, type SingleChoiceProps, type MultipleChoiceProps } from "./state";

type Props = ChoiceProps & { multiple: boolean; values: readonly string[]; onValuesChange(values: string[]): void };
function ChoiceSelect(props: Props) {
  const field = fieldAccessibility(props);
  const navigation = useNavigation();
  let root!: HTMLDivElement, popup!: HTMLDivElement, list!: HTMLDivElement, footer!: HTMLDivElement, trigger!: HTMLButtonElement;
  const session = new ChoiceSession(() => props.values, () => props.multiple, values => props.onValuesChange(values));
  const items = () => [...props.items];
  const collection = createMemo(() => createListCollection({ items: items(), groupBy: item => item.group ?? "" }));
  const api = useSelect<ChoiceItem>(() => ({ scrollToIndexFn: details => details.getElement()?.scrollIntoView?.({ block: "nearest" }), ids: { trigger: `${field.id}-control` }, collection: collection(), value: session.value(), multiple: props.multiple, composite: false, open: session.open(), closeOnSelect: !props.multiple, disabled: props.disabled, readOnly: props.readOnly, required: props.required, invalid: !!props.error, name: props.name,
    onOpenChange: details => details.open ? session.begin() : session.close(), onValueChange: details => session.change(details.value), positioning: { placement: "bottom-end", gutter: 8 } }));
  const summary = () => props.values.map(value => props.items.find(item => item.value === value)?.label ?? value).join(", ") || props.placeholder || "Choose…";
  createEffect(() => {
    if (!session.open()) return;
    let active = true; onCleanup(() => { active = false; });
    queueMicrotask(() => { if (active) api().setHighlightValue(items().find(item => !item.disabled && session.value().includes(item.value))?.value ?? items().find(item => !item.disabled)?.value ?? ""); });
  });
  createEffect(() => {
    api().highlightedValue;
    let active = true; onCleanup(() => { active = false; });
    queueMicrotask(() => { if (active) list?.querySelector<HTMLElement>("[data-highlighted]")?.scrollIntoView?.({ block: "nearest" }); });
  });
  useFocusRing(() => list, () => { api().highlightedValue; return session.open() ? list?.querySelector("[data-highlighted]") : null; });
  useInputScope(() => ({ root, handle: action => {
    if (action.origin !== "controller" || action.type !== "activate") return false;
    if (!props.disabled && !props.readOnly) session.begin();
    return true;
  } }));
  useInputScope(() => ({ root: popup, enabled: session.open(), overlay: true, navigation: true, initialFocus: () => list, returnFocus: () => trigger,
    hints: () => [{ action: "activate", label: props.multiple ? "Toggle" : "Select" }, { action: "back", label: "Cancel" }],
    handle: action => {
      if (action.origin === "keyboard" && action.type !== "back") return false;
      const wasFooter = !!footer?.contains(document.activeElement);
      const handled = session.navigate(action, api(), items(), props.multiple ? footer : undefined);
      if (handled && wasFooter && action.type === "move" && action.direction === "up") list.focus();
      return handled;
    } }));
  onMount(() => {
    const navigateClosedPicker = (event: KeyboardEvent) => {
      if (session.open() || event.defaultPrevented || event.isComposing || event.altKey || event.ctrlKey || event.metaKey) return;
      const directions: Record<string, Direction> = { ArrowUp: "up", ArrowDown: "down", ArrowLeft: "left", ArrowRight: "right" };
      const direction = directions[event.key];
      if (!direction) return;
      // A closed picker is a navigation stop. Ark owns keys after it opens;
      // its closed-trigger arrow handlers would otherwise trap focus or edit values.
      navigation.dispatch({ type: "move", direction, origin: "keyboard", sourceId: null, event });
      event.preventDefault();
    };
    trigger.addEventListener("keydown", navigateClosedPicker, true);
    onCleanup(() => trigger.removeEventListener("keydown", navigateClosedPicker, true));
  });
  return <Field {...props} id={field.id}><Ark.RootProvider ref={root} value={api} class="hz-choice"><Ark.Control><Ark.Trigger {...field.attributes()} ref={trigger} class="hz-choice-trigger" title={summary()}><span>{summary()}</span></Ark.Trigger></Ark.Control><Ark.HiddenSelect class="hz-hidden-input" ref={element => {
      // The embedded DOM implements select values but not HTMLOptionsCollection.
      const select = element as HTMLElement;
      if (!("options" in select)) Object.defineProperty(select, "options", { get: () => [...select.children].filter(child => child.tagName === "OPTION") });
    }} />
    <OverlayPortal><Ark.Positioner class="hz-picker-positioner"><Ark.Content ref={popup} class="hz-popup" tabIndex={-1}>
      <Show when={props.loading}><p role="status">Loading…</p></Show><Show when={props.loadError}><p role="alert">{props.loadError}</p></Show>
      <Ark.List ref={list} class="hz-choice-list"><For each={collection().group()}>{([group, entries]) => <Ark.ItemGroup><Show when={group}><Ark.ItemGroupLabel class="hz-option-group">{group}</Ark.ItemGroupLabel></Show><For each={entries}>{item => <Ark.Item item={item} class="hz-option" title={item.label}><span><Ark.ItemText>{item.label}</Ark.ItemText><Show when={item.description}><small>{item.description}</small></Show></span><span class="hz-option-mark" data-multiple={props.multiple || undefined}><Ark.ItemIndicator>{props.multiple ? "✓" : "●"}</Ark.ItemIndicator></span></Ark.Item>}</For></Ark.ItemGroup>}</For></Ark.List>
      <Show when={!props.loading && !props.loadError && !props.items.length}><p role="status">No options available</p></Show>
      <Show when={props.multiple}><div ref={footer} class="hz-picker-actions" onKeyDown={event => { if (event.key !== "Escape") event.stopPropagation(); }}><Button appearance="primary" onClick={session.apply}>Done</Button><Button onClick={session.close}>Cancel</Button></div></Show>
    </Ark.Content></Ark.Positioner></OverlayPortal>
  </Ark.RootProvider></Field>;
}
export function Select(props: SingleChoiceProps) { return <ChoiceSelect {...props} multiple={false} values={props.value === null ? [] : [props.value]} onValuesChange={values => props.onValueChange(values[0] ?? null)} />; }
export function MultiSelect(props: MultipleChoiceProps) { return <ChoiceSelect {...props} multiple values={props.value} onValuesChange={props.onValueChange} />; }
