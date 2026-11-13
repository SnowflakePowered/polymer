import { For, Show, createSignal, onCleanup, onMount } from "solid-js";
import type { InputLayout } from "@polymer-fe/frontend";
import { BindingList } from "./BindingList";
import { SourceChip } from "./SourceChip";
import { outlines, type Callout, type Outline } from "./outlines";
import type { PadFamily } from "./padGlyphs";

type GuideProps = { onSelect(keys: readonly string[]): void; family: PadFamily; layout: InputLayout; bindings: Readonly<Record<string, string>>; overrides: Readonly<Record<string, string>> };

const PITCH = 1.35;
const ELBOW = 26;
const MIN_HEIGHT = 340;
const CHROME = 280;
const CHIP_HEIGHT = 36;
const BASE_VIEWPORT = 720;
export const uiScale = (viewport: number) => Math.min(2, Math.max(1, viewport / BASE_VIEWPORT));
const DIRECTION = /\s+(Up|Down|Left|Right)$/;

function OutlineGuide(props: GuideProps & { outline: Outline }) {
  let root!: HTMLDivElement;
  const [width, setWidth] = createSignal(0);
  const [viewport, setViewport] = createSignal(window.innerHeight);
  onMount(() => {
    const observer = new ResizeObserver(() => setWidth(root.clientWidth));
    observer.observe(root);
    setWidth(root.clientWidth);
    const resize = () => setViewport(window.innerHeight);
    window.addEventListener("resize", resize);
    onCleanup(() => { observer.disconnect(); window.removeEventListener("resize", resize); });
  });
  const columns = () => ({ left: props.outline.callouts.filter(callout => callout.side === "left"), right: props.outline.callouts.filter(callout => callout.side === "right") });
  const scaleUi = () => uiScale(viewport());
  const chipWidth = () => Math.max(160, Math.min(260, width() * 0.22)) * scaleUi();
  const chipHeight = () => CHIP_HEIGHT * scaleUi();
  const nameOf = (callout: Callout) => {
    const labels = callout.keys.map(key => props.layout.elements.find(element => element.key === key)?.label ?? key.replace(/^trigger\.(left|right)$/, (_, side: string) => `${side === "left" ? "Left" : "Right"} Trigger`));
    return callout.keys.length > 1 ? labels[0].replace(DIRECTION, "") : labels[0];
  };
  const pitch = () => chipHeight() * PITCH;
  const rows = () => Math.max(columns().left.length, columns().right.length);
  const availableWidth = () => Math.max(0, Math.min(width() - 2 * (chipWidth() + 90), width() * 0.5));
  const scale = () => Math.min(availableWidth() / props.outline.size[0], Math.max(MIN_HEIGHT, viewport() - CHROME) / props.outline.size[1]);
  const outlineWidth = () => props.outline.size[0] * scale();
  const outlineHeight = () => props.outline.size[1] * scale();
  const height = () => Math.max(rows() * pitch(), outlineHeight() + 2 * pitch());
  const originX = () => (width() - outlineWidth()) / 2;
  const originY = () => (height() - outlineHeight()) / 2;
  const changed = (callout: Callout) => callout.keys.some(key => key in props.overrides);
  const placed = () => (["left", "right"] as const).flatMap(side => columns()[side].map((callout, index, all) => ({
    callout,
    x: side === "left" ? chipWidth() / 2 : width() - chipWidth() / 2,
    y: (height() - all.length * pitch()) / 2 + (index + 0.5) * pitch(),
  })));
  const points = (item: ReturnType<typeof placed>[number]) => {
    const direction = item.callout.side === "left" ? 1 : -1;
    const edge = item.x + direction * chipWidth() / 2;
    return `${edge},${item.y} ${edge + direction * ELBOW},${item.y} ${originX() + item.callout.at[0] * scale()},${originY() + item.callout.at[1] * scale()}`;
  };

  return <div ref={root} class="vp-button-guide" style={{ height: `${height()}px`, "--chip-height": `${chipHeight()}px`, "--k": scaleUi() }}>
    <div class="vp-guide-outline" innerHTML={props.outline.svg} style={{ left: `${originX()}px`, top: `${originY()}px`, width: `${outlineWidth()}px`, height: `${outlineHeight()}px` }} />
    <svg class="vp-guide-lines" width={width()} height={height()} viewBox={`0 0 ${width()} ${height()}`}>
      <For each={placed()}>{item => <polyline points={points(item)} fill="none" stroke="currentColor" stroke-width="1" style={{ color: changed(item.callout) ? "var(--hz-action)" : "var(--hz-muted)" }} />}</For>
    </svg>
    <For each={placed()}>{item => <button type="button" class="vp-callout" data-side={item.callout.side} onClick={() => props.onSelect(item.callout.keys)} data-changed={changed(item.callout) || undefined}
      style={{ left: `${item.x}px`, top: `${item.y}px`, width: `${chipWidth()}px`, height: `${chipHeight()}px` }}>
      <span class="vp-callout-name">{nameOf(item.callout)}</span><SourceChip family={props.family} sources={item.callout.keys.map(key => props.bindings[key] ?? "none")} />
    </button>}</For>
  </div>;
}

export function ButtonGuide(props: GuideProps) {
  return <Show when={outlines[props.layout.id]} fallback={<BindingList {...props} />}>{outline => <OutlineGuide {...props} outline={outline()} />}</Show>;
}

export function OutlinePreview(props: { layoutId: string }) {
  return <div class="vp-outline-preview" innerHTML={outlines[props.layoutId]?.svg ?? ""} />;
}
