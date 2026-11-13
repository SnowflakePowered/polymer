import { For, Show } from "solid-js";
import type { InputLayout } from "@polymer-fe/frontend";
import { SourceChip } from "./SourceChip";
import type { PadFamily } from "./padGlyphs";

const GROUPS: readonly (readonly [string, readonly string[]])[] = [
  ["Buttons", ["Button"]],
  ["D-pad", ["Directional"]],
  ["Sticks", ["AxisPositive", "AxisNegative"]],
  ["Triggers", ["Trigger"]],
];

export function BindingList(props: { onSelect(keys: readonly string[]): void; family: PadFamily; layout: InputLayout; bindings: Readonly<Record<string, string>>; overrides: Readonly<Record<string, string>> }) {
  return <div class="vp-binding-list">
    <For each={GROUPS}>{([title, kinds]) => {
      const elements = () => props.layout.elements.filter(element => kinds.includes(element.kind));
      return <Show when={elements().length}><section class="vp-binding-group">
        <h3>{title}</h3>
        <For each={elements()}>{element => <button type="button" class="vp-binding-row" onClick={() => props.onSelect([element.key])} data-changed={element.key in props.overrides || undefined}>
          <span>{element.label}</span>
          <span class="vp-callout"><SourceChip family={props.family} sources={[props.bindings[element.key] ?? "none"]} /></span>
        </button>}</For>
      </section></Show>;
    }}</For>
  </div>;
}
