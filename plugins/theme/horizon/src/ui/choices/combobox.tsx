import { Combobox as Ark, createListCollection, useCombobox } from "@ark-ui/solid";
import { For, Show, createEffect, createMemo, onCleanup } from "solid-js";
import { Button } from "../actions";
import { Field, fieldAccessibility } from "../forms";
import { useFocusRing, useInputScope } from "../input";
import { OverlayPortal } from "../portal";
import { ChoiceSession, type ChoiceItem, type SingleChoiceProps, type MultipleChoiceProps } from "./state";

export type ComboboxProps = (SingleChoiceProps & { multiple?: false }) | (MultipleChoiceProps & { multiple: true });
export function Combobox(props: ComboboxProps) {
  const field = fieldAccessibility(props);
  let root!: HTMLDivElement, popup!: HTMLDivElement, input!: HTMLInputElement, footer!: HTMLDivElement;
  const values = () => props.multiple ? [...props.value] : props.value === null ? [] : [props.value];
  const session = new ChoiceSession(values, () => !!props.multiple, selected => { if (props.multiple) props.onValueChange(selected); else props.onValueChange(selected[0] ?? null); });
  const filtered = createMemo(() => props.items.filter(item => item.label.toLocaleLowerCase().includes(session.query().toLocaleLowerCase())));
  const collection = createMemo(() => createListCollection({ items: filtered(), groupBy: item => item.group ?? "" }));
  const api = useCombobox<ChoiceItem>(() => ({ scrollToIndexFn: details => details.getElement()?.scrollIntoView?.({ block: "nearest" }), ids: { input: `${field.id}-control` }, collection: collection(), value: session.value(), multiple: !!props.multiple, composite: false, open: session.open(), inputValue: session.query(), inputBehavior: "none", selectionBehavior: "clear", closeOnSelect: !props.multiple, disabled: props.disabled, readOnly: props.readOnly, required: props.required, invalid: !!props.error, name: props.name,
    onOpenChange: details => details.open ? session.begin() : session.close(), onValueChange: details => session.change(details.value), onInputValueChange: details => session.setQuery(details.inputValue), positioning: { placement: "bottom-end", gutter: 8 } }));
  const summary = () => values().map(value => props.items.find(item => item.value === value)?.label ?? value).join(", ") || props.placeholder || "Choose…";
  const compactSummary = () => {
    const selected = values();
    if (selected.length < 2) return summary();
    const first = props.items.find(item => item.value === selected[0])?.label ?? selected[0];
    return `${first} +${selected.length - 1}`;
  };
  createEffect(() => {
    const open = session.open(), options = filtered();
    let active = true; onCleanup(() => { active = false; });
    if (open) queueMicrotask(() => {
      if (!active) return;
      const selected = options.find(item => !item.disabled && session.value().includes(item.value));
      api().setHighlightValue(selected?.value ?? options.find(item => !item.disabled)?.value ?? "");
    });
  });
  createEffect(() => {
    api().highlightedValue;
    let active = true; onCleanup(() => { active = false; });
    queueMicrotask(() => { if (active) popup?.querySelector<HTMLElement>("[data-highlighted]")?.scrollIntoView?.({ block: "nearest" }); });
  });
  useFocusRing(() => input, () => { api().highlightedValue; return session.open() ? popup?.querySelector("[data-highlighted]") : null; });
  useInputScope(() => ({ root, enabled: !session.open(), handle: action => {
    if (action.origin !== "controller" || action.type !== "activate") return false;
    if (!props.disabled && !props.readOnly) session.begin();
    return true;
  } }));
  useInputScope(() => ({ root: popup, enabled: session.open(), overlay: true, branches: () => [root], navigation: true, initialFocus: () => input, returnFocus: () => input,
    hints: () => [{ action: "activate", label: props.multiple ? "Toggle" : "Select" }, { action: "back", label: "Cancel" }],
    handle: action => {
      if (action.origin === "keyboard" && action.type !== "back") return false;
      const wasFooter = !!footer?.contains(document.activeElement);
      const handled = session.navigate(action, api(), filtered(), props.multiple ? footer : undefined);
      if (handled && wasFooter && action.type === "move" && action.direction === "up") input.focus();
      return handled;
    } }));
  return <Field {...props} id={field.id}><Ark.RootProvider ref={root} value={api} class="hz-choice"><Ark.Control class="hz-combobox-control"><Ark.Input {...field.attributes()} ref={input} placeholder={session.open() ? "Search options…" : compactSummary()} title={summary()} aria-label={`${props.label}, search options`} onClick={() => { if (!props.disabled && !props.readOnly) session.begin(); }} /></Ark.Control>
    <span class="hz-choice-announcement" aria-live="polite">{summary()}</span>
    <OverlayPortal><Ark.Positioner class="hz-picker-positioner"><Ark.Content ref={popup} class="hz-popup">
      <Show when={props.loading}><p role="status">Loading…</p></Show><Show when={props.loadError}><p role="alert">{props.loadError}</p></Show>
      <Ark.List class="hz-choice-list"><For each={collection().group()}>{([group, entries]) => <Ark.ItemGroup><Show when={group}><Ark.ItemGroupLabel class="hz-option-group">{group}</Ark.ItemGroupLabel></Show><For each={entries}>{item => <Ark.Item item={item} class="hz-option" title={item.label}><span><Ark.ItemText>{item.label}</Ark.ItemText><Show when={item.description}><small>{item.description}</small></Show></span><span class="hz-option-mark" data-multiple={props.multiple || undefined}><Ark.ItemIndicator>{props.multiple ? "✓" : "●"}</Ark.ItemIndicator></span></Ark.Item>}</For></Ark.ItemGroup>}</For></Ark.List>
      <Show when={!props.loading && !props.loadError && !filtered().length}><p role="status">No matching options</p></Show>
      <Show when={props.multiple}><div ref={footer} class="hz-picker-actions" onPointerDown={event => event.stopPropagation()}><Button appearance="primary" onClick={session.apply}>Done</Button><Button onClick={session.close}>Cancel</Button></div></Show>
    </Ark.Content></Ark.Positioner></OverlayPortal>
  </Ark.RootProvider></Field>;
}
