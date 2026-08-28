import { Field, NumberInput, Select, Slider, Switch, createListCollection } from "@ark-ui/solid";
import { For, Show, createMemo } from "solid-js";
import type { ConfigurationOption } from "../../../../../js/snowglass";
import { OverlayPortal } from "./OverlayPortal";
import "./FormControls.css";

export type ChoiceItem = { value: string; label: string };

export function ChoiceControl(props: {
  label: string;
  value: string;
  items: ChoiceItem[];
  onChange(value: string): void;
}) {
  const collection = createMemo(() => createListCollection({ items: props.items }));
  return <Select.Root collection={collection()} value={[props.value]} onValueChange={(details: any) => props.onChange(details.value[0] || "")} positioning={{ sameWidth: true }}>
    <Select.Label class="horizon-field-label">{props.label}</Select.Label>
    <Select.Control>
      <Select.Trigger class="horizon-select-trigger"><Select.ValueText /><Select.Indicator>⌄</Select.Indicator></Select.Trigger>
    </Select.Control>
    <OverlayPortal><Select.Positioner class="horizon-select-positioner">
        <Select.Content class="horizon-select-content">
          <For each={props.items}>{(item) => <Select.Item item={item} class="horizon-select-item"><Select.ItemText>{item.label}</Select.ItemText><Select.ItemIndicator>✓</Select.ItemIndicator></Select.Item>}</For>
        </Select.Content>
      </Select.Positioner></OverlayPortal>
  </Select.Root>;
}

export function OptionControl(props: { option: ConfigurationOption; onChange(value: unknown): void }) {
  const numeric = () => props.option.kind === "integer" || props.option.kind === "float";
  return <Show when={props.option.kind !== "boolean"} fallback={
    <Switch.Root class="horizon-switch" checked={Boolean(props.option.value)} onCheckedChange={(details: any) => props.onChange(details.checked)}>
      <Switch.Label>{props.option.title}</Switch.Label>
      <Switch.Control><Switch.Thumb /></Switch.Control>
    </Switch.Root>
  }>
    <Show when={props.option.kind === "select"} fallback={
      <Show when={numeric()} fallback={
        <Field.Root class="horizon-field"><Field.Label class="horizon-field-label">{props.option.title}</Field.Label><Field.Input value={String(props.option.value ?? "")} onInput={(event: InputEvent & { currentTarget: HTMLInputElement }) => props.onChange(event.currentTarget.value)} /></Field.Root>
      }>
        <NumberInput.Root class="horizon-number" value={String(props.option.value ?? 0)} min={props.option.min} max={props.option.max} onValueChange={(details: any) => props.onChange(details.valueAsNumber)}>
          <NumberInput.Label class="horizon-field-label">{props.option.title}</NumberInput.Label>
          <NumberInput.Control><NumberInput.Input /><NumberInput.DecrementTrigger>−</NumberInput.DecrementTrigger><NumberInput.IncrementTrigger>＋</NumberInput.IncrementTrigger></NumberInput.Control>
        </NumberInput.Root>
      </Show>
    }>
      <ChoiceControl label={props.option.title} value={String(props.option.value ?? "")} items={(props.option.choices || []).map((choice) => ({ value: choice.key, label: choice.label }))} onChange={props.onChange} />
    </Show>
  </Show>;
}

export function VolumeSlider(props: { value: number; onChange(value: number): void }) {
  return <Slider.Root class="horizon-slider" min={0} max={1} step={0.01} value={[props.value]} onValueChange={(details: any) => props.onChange(details.value[0])}>
    <Slider.Label>Volume</Slider.Label>
    <Slider.Control><Slider.Track><Slider.Range /></Slider.Track><Slider.Thumb index={0} /></Slider.Control>
  </Slider.Root>;
}
