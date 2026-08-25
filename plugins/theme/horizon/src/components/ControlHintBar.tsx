import { For } from "solid-js";
import "./ControlHintBar.css";

export type ControlHint = { key: string; label: string };
export function ControlHintBar(props: { hints: ControlHint[] }) {
  return <footer class="horizon-hints"><For each={props.hints}>{(hint) => <span class="horizon-hint"><kbd>{hint.key}</kbd>{hint.label}</span>}</For></footer>;
}
