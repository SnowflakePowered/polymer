import { Match, Switch } from "solid-js";
import type { ConfigurationLayoutOption } from "@polymer-fe/frontend";
import { NumberField, Select, Slider, TextField, Toggle } from "../../ui";

/** Adapter from the emulator's scalar configuration schema into UI fields. */
export function OptionControl(props: { option: ConfigurationLayoutOption; value: boolean | number | string | null; onChange(value: boolean | number | string | null): void }) {
  return <Switch fallback={<TextField label={props.option.title} description={props.option.description} value={String(props.value ?? "")} onValueChange={props.onChange} />}>
    <Match when={props.option.type === "bool"}><Toggle label={props.option.title} description={props.option.description} checked={Boolean(props.value)} onCheckedChange={props.onChange} /></Match>
    <Match when={props.option.type === "int" && props.option.min !== undefined && props.option.max !== undefined}><Slider label={props.option.title} description={props.option.description} value={Number(props.value ?? props.option.min)} min={props.option.min} max={props.option.max} step={1} onValueChange={props.onChange} /></Match>
    <Match when={props.option.type === "int" || props.option.type === "float"}><NumberField label={props.option.title} description={props.option.description} value={Number(props.value ?? 0)} min={props.option.min} max={props.option.max} step={props.option.type === "int" ? 1 : .1} onValueChange={props.onChange} /></Match>
    <Match when={props.option.type === "select"}><Select label={props.option.title} description={props.option.description} value={props.value === null ? null : String(props.value)} items={(props.option.choices ?? []).map(choice => ({ value: choice.key, label: choice.label }))} onValueChange={props.onChange} /></Match>
  </Switch>;
}
