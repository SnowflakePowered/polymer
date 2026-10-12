import { Show } from "solid-js";
import "./DualScreenStage.css";

export const DUAL_SCREEN_PLATFORMS = ["NINTENDO_3DS", "NINTENDO_N3DS"];
export const DUAL_SCREEN_LAYOUTS = ["stacked", "side-by-side", "hybrid"] as const;
export type DualScreenLayout = typeof DUAL_SCREEN_LAYOUTS[number];

export function DualScreenStage(props: {
  layout: DualScreenLayout;
  register(index: number, element: HTMLElement): void;
  registerGroup(element: HTMLElement): void;
}) {
  return <section class="horizon-dual-screen-stage">
    <tracked-surface-group ref={props.registerGroup} class="horizon-dual-screen-frame" attr:data-layout={props.layout}>
      <tracked-surface ref={(element: HTMLElement) => props.register(0, element)} src="0" data-index="0" class="horizon-game-surface horizon-dual-screen" attr:data-screen="top" />
      <tracked-surface ref={(element: HTMLElement) => props.register(1, element)} src="1" data-index="1" class="horizon-game-surface horizon-dual-screen" attr:data-screen="bottom" />
      <Show when={props.layout === "hybrid"}>
        <tracked-surface attr:src="0" class="horizon-game-surface horizon-dual-screen" attr:data-screen="mirror" />
      </Show>
    </tracked-surface-group>
  </section>;
}
