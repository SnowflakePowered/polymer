import { For, Show } from "solid-js";
import { useMatch } from "@solidjs/router";
import { useShell } from "../../app/shell-context";
import { DUAL_SCREEN_LAYOUTS, DUAL_SCREEN_PLATFORMS, type DualScreenLayout } from "./DualScreenStage";
import "./ScreenLayoutPicker.css";

const LABELS: Record<DualScreenLayout, string> = {
  stacked: "Top and bottom",
  "side-by-side": "Side by side",
  hybrid: "Hybrid",
};

export function ScreenLayoutPicker() {
  const shell = useShell();
  const dualScreen = useMatch(() => "/game/:platform/*", { platform: DUAL_SCREEN_PLATFORMS });
  const route = useMatch(() => "/game/:platform/:game/play/:layout", { layout: [...DUAL_SCREEN_LAYOUTS] });
  const current = () => route()?.params.layout ?? "stacked";
  return <Show when={dualScreen()}>
    <div class="horizon-layout-picker" role="radiogroup" aria-label="Screen layout">
      <For each={DUAL_SCREEN_LAYOUTS}>{(layout) =>
        <button type="button" role="radio" class="horizon-layout-choice" data-select-ring aria-checked={current() === layout} aria-label={LABELS[layout]} title={LABELS[layout]}
          onClick={() => shell.navigate(`${shell.gamePath("play")}/${layout}`, true)}>
          <span class="horizon-layout-glyph" data-layout={layout}>
            <span data-screen="top" />
            <span data-screen="bottom" />
            <Show when={layout === "hybrid"}><span data-screen="mirror" /></Show>
          </span>
        </button>
      }</For>
    </div>
  </Show>;
}
