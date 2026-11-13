import { For, Show } from "solid-js";
import { keyGlyph } from "./keyGlyphs";
import { padGlyph, type PadFamily } from "./padGlyphs";

const PAD_LABELS: Readonly<Record<string, string>> = {
  "button.start": "Start", "button.select": "Select", "button.guide": "Guide",
  "button.clickl": "L3", "button.clickr": "R3", "trigger.left": "LT", "trigger.right": "RT",
  "button.l": "L", "button.r": "R", "button.a": "A", "button.b": "B", "button.x": "X", "button.y": "Y",
};

export function sourceLabel(source: string) {
  if (source === "none") return "—";
  if (source.startsWith("key.")) return source.slice(4).replace(/^(Key|Digit|Numpad)/, "");
  if (source.startsWith("mouse.button.")) return `M${Number(source.slice(13)) + 1}`;
  if (source.startsWith("mouse.")) return source.slice(6).replace(".", " ");
  if (source.startsWith("analog.stick.")) return source.includes(".left.") ? "LS" : "RS";
  if (source.startsWith("directional.")) return "D-pad";
  return PAD_LABELS[source] ?? source;
}

function Single(props: { source: string; family: PadFamily }) {
  const image = () => padGlyph(props.family, props.source) ?? keyGlyph(props.source);
  return <Show when={image()} fallback={<kbd>{sourceLabel(props.source)}</kbd>}>{src => <img class="vp-glyph" src={src()} alt={sourceLabel(props.source)} />}</Show>;
}

export function SourceChip(props: { sources: readonly string[]; family: PadFamily }) {
  const grouped = () => new Set(props.sources.map(source => padGlyph(props.family, source) ?? keyGlyph(source) ?? sourceLabel(source))).size > 1;
  return <Show when={grouped()} fallback={<Single source={props.sources[0] ?? "none"} family={props.family} />}>
    <span class="vp-source-cluster"><For each={props.sources}>{source => <Single source={source} family={props.family} />}</For></span>
  </Show>;
}
