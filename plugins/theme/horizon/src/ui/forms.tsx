import { Field as ArkField, NumberInput, Switch, Checkbox as ArkCheckbox, RadioGroup as ArkRadio, Slider as ArkSlider, useNumberInput, useSwitch, useCheckbox, useSlider, useRadioGroup } from "@ark-ui/solid";
import { For, Show, createUniqueId, onCleanup, onMount, type ParentProps } from "solid-js";
import { useInputScope, useNavigation, type Direction } from "./input";

export type FieldProps = { id?: string; label: string; description?: string; error?: string; disabled?: boolean; readOnly?: boolean; required?: boolean; name?: string };
/** Stable label and error associations also cover hooks created outside Field's context. */
export function fieldAccessibility(props: FieldProps) {
  const id = props.id ?? createUniqueId();
  return { id, attributes: () => ({ id: `${id}-control`, "aria-label": props.label, "aria-invalid": !!props.error, "aria-describedby": [props.description && `${id}-help`, props.error && `${id}-error`].filter(Boolean).join(" ") || undefined }) };
}
export function Field(props: ParentProps<FieldProps>) {
  const id = props.id ?? createUniqueId();
  return <ArkField.Root class="hz-field" disabled={props.disabled} readOnly={props.readOnly} invalid={!!props.error} required={props.required}>
    <ArkField.Label for={`${id}-control`}>{props.label}<ArkField.RequiredIndicator /></ArkField.Label>{props.children}
    <Show when={props.description}><ArkField.HelperText id={`${id}-help`}>{props.description}</ArkField.HelperText></Show>
    <Show when={props.error}><ArkField.ErrorText id={`${id}-error`}>{props.error}</ArkField.ErrorText></Show>
  </ArkField.Root>;
}
export function TextField(props: FieldProps & { value: string; placeholder?: string; onValueChange(value: string): void }) {
  const field = fieldAccessibility(props);
  return <Field {...props} id={field.id}><ArkField.Input {...field.attributes()} name={props.name} value={props.value} placeholder={props.placeholder} onInput={event => props.onValueChange(event.currentTarget.value)} /></Field>;
}
export function NumberField(props: FieldProps & { value: number; min?: number; max?: number; step?: number; onValueChange(value: number): void }) {
  const field = fieldAccessibility(props);
  let root!: HTMLDivElement;
  const api = useNumberInput(() => ({ ids: { input: `${field.id}-control` }, value: String(props.value), min: props.min, max: props.max, step: props.step ?? 1, disabled: props.disabled, readOnly: props.readOnly, required: props.required, invalid: !!props.error, name: props.name, onValueChange: details => { if (Number.isFinite(details.valueAsNumber)) props.onValueChange(details.valueAsNumber); } }));
  useInputScope(() => ({ root, handle: action => {
    if (action.origin !== "controller" || action.type !== "move" || props.disabled || props.readOnly) return false;
    if (action.direction === "left") { api().decrement(); return true; }
    if (action.direction === "right") { api().increment(); return true; }
    return false;
  } }));
  return <Field {...props} id={field.id}><NumberInput.RootProvider ref={root} value={api} class="hz-number"><NumberInput.Control><NumberInput.DecrementTrigger aria-label={`Decrease ${props.label}`}>−</NumberInput.DecrementTrigger><NumberInput.Input {...field.attributes()} /><NumberInput.IncrementTrigger aria-label={`Increase ${props.label}`}>+</NumberInput.IncrementTrigger></NumberInput.Control></NumberInput.RootProvider></Field>;
}
const nativeCheckedInput = (props: FieldProps, setChecked: (checked: boolean) => void) => (event: InputEvent) => {
  if (!props.disabled && !props.readOnly && (event.data === "true" || event.data === "false")) setChecked(event.data === "true");
};
export function Toggle(props: FieldProps & { checked: boolean; onCheckedChange(checked: boolean): void }) {
  const field = fieldAccessibility(props);
  let root!: HTMLLabelElement;
  const api = useSwitch(() => ({ ids: { hiddenInput: `${field.id}-control` }, checked: props.checked, disabled: props.disabled, readOnly: props.readOnly, required: props.required, invalid: !!props.error, name: props.name, onCheckedChange: details => props.onCheckedChange(details.checked) }));
  useInputScope(() => ({ root, handle: action => {
    if (action.origin !== "controller" || action.type !== "activate") return false;
    if (!props.disabled && !props.readOnly) api().toggleChecked();
    return true;
  } }));
  return <Field {...props} id={field.id}><Switch.RootProvider ref={root} value={api} class="hz-toggle"><Switch.Control><Switch.Thumb /></Switch.Control><Switch.HiddenInput {...field.attributes()} class="hz-hidden-input" onInput={nativeCheckedInput(props, checked => api().setChecked(checked))} /></Switch.RootProvider></Field>;
}
export function Checkbox(props: FieldProps & { checked: boolean | "indeterminate"; onCheckedChange(checked: boolean | "indeterminate"): void }) {
  const field = fieldAccessibility(props);
  let root!: HTMLLabelElement;
  const api = useCheckbox(() => ({ ids: { hiddenInput: `${field.id}-control` }, checked: props.checked, disabled: props.disabled, readOnly: props.readOnly, required: props.required, invalid: !!props.error, name: props.name, onCheckedChange: details => props.onCheckedChange(details.checked) }));
  useInputScope(() => ({ root, handle: action => {
    if (action.origin !== "controller" || action.type !== "activate") return false;
    if (!props.disabled && !props.readOnly) api().toggleChecked();
    return true;
  } }));
  return <Field {...props} id={field.id}><ArkCheckbox.RootProvider ref={root} value={api} class="hz-checkbox"><ArkCheckbox.Control><ArkCheckbox.Indicator><svg width="18" height="18" viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m4 9 3 3 7-7" /></svg></ArkCheckbox.Indicator><ArkCheckbox.Indicator indeterminate><svg width="18" height="18" viewBox="0 0 18 18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><path d="M4 9h10" /></svg></ArkCheckbox.Indicator></ArkCheckbox.Control><ArkCheckbox.HiddenInput {...field.attributes()} class="hz-hidden-input" onInput={nativeCheckedInput(props, checked => api().setChecked(checked))} aria-checked={props.checked === "indeterminate" ? "mixed" : props.checked} /></ArkCheckbox.RootProvider></Field>;
}
export function RadioGroup(props: FieldProps & { value: string | null; items: readonly { value: string; label: string; disabled?: boolean }[]; onValueChange(value: string): void }) {
  const field = fieldAccessibility(props);
  let root!: HTMLDivElement;
  const api = useRadioGroup(() => ({ ids: { root: `${field.id}-control` }, value: props.value, disabled: props.disabled, readOnly: props.readOnly, required: props.required, invalid: !!props.error, name: props.name, onValueChange: details => { if (details.value) props.onValueChange(details.value); } }));
  useInputScope(() => ({ root, handle: action => {
    if (action.origin !== "controller" || action.type !== "move" || props.disabled || props.readOnly) return false;
    if (action.direction !== "left" && action.direction !== "right") return false;
    const enabled = props.items.filter(item => !item.disabled);
    const index = enabled.findIndex(item => item.value === props.value);
    const item = enabled[Math.max(0, Math.min(enabled.length - 1, index + (action.direction === "left" ? -1 : 1)))];
    if (item) api().setValue(item.value);
    return true;
  } }));
  return <Field {...props} id={field.id}><ArkRadio.RootProvider {...field.attributes()} ref={root} value={api} class="hz-radio"><For each={props.items}>{item => <ArkRadio.Item value={item.value} disabled={item.disabled}><ArkRadio.ItemControl /><ArkRadio.ItemText>{item.label}</ArkRadio.ItemText><ArkRadio.ItemHiddenInput class="hz-hidden-input" /></ArkRadio.Item>}</For></ArkRadio.RootProvider></Field>;
}
export function Slider(props: FieldProps & { value: number; min?: number; max?: number; step?: number; onValueChange(value: number): void }) {
  const field = fieldAccessibility(props);
  const navigation = useNavigation();
  let root!: HTMLDivElement;
  const api = useSlider(() => ({ ids: { thumb: () => `${field.id}-control` }, value: [props.value], min: props.min ?? 0, max: props.max ?? 100, step: props.step ?? 1, disabled: props.disabled, readOnly: props.readOnly, required: props.required, invalid: !!props.error, name: props.name, onValueChange: details => props.onValueChange(details.value[0]) }));
  useInputScope(() => ({ root, handle: action => {
    if (action.type !== "move" || props.disabled || props.readOnly) return false;
    if (action.direction === "left") { api().decrement(0); return true; }
    if (action.direction === "right") { api().increment(0); return true; }
    return false;
  } }));
  onMount(() => {
    const directions: Record<string, Direction> = { ArrowUp: "up", ArrowDown: "down", ArrowLeft: "left", ArrowRight: "right" };
    const keydown = (event: KeyboardEvent) => {
      if (!directions[event.key]) return;
      navigation.dispatch({ type: "move", direction: directions[event.key], origin: "keyboard", sourceId: null, event });
      event.stopPropagation();
    };
    const focusin = (event: FocusEvent) => {
      event.stopPropagation();
      navigation.focusChanged();
    };
    root.addEventListener("keydown", keydown, true);
    root.addEventListener("focusin", focusin, true);
    onCleanup(() => { root.removeEventListener("keydown", keydown, true); root.removeEventListener("focusin", focusin, true); });
  });
  return <Field {...props} id={field.id}><ArkSlider.RootProvider ref={root} value={api} class="hz-slider"><ArkSlider.Control><ArkSlider.Track><ArkSlider.Range /></ArkSlider.Track><ArkSlider.Thumb index={0} data-select-ring {...field.attributes()}><ArkSlider.HiddenInput class="hz-hidden-input" /></ArkSlider.Thumb></ArkSlider.Control><ArkSlider.ValueText /></ArkSlider.RootProvider></Field>;
}
