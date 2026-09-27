import { FloatingPanel } from "../../ui";
import { For, Show, createSignal } from "solid-js";
import { useMatch } from "@solidjs/router";
import type { LaunchRequest } from "../../../../../../js/polymer";
import { useShell } from "../../app/shell-context";
import { Icon } from "../../ui";
import { ClassicSurfaceBackdrop } from "./ClassicSurfaceBackdrop";
import "./SurfaceStage.css";

type Surface = LaunchRequest["surfaces"][number];

const POINTER_PLATFORMS = new Set(["NINTENDO_WII", "NINTENDO_WIIU"]);

export function SurfaceStage(props: {
  surfaces: Surface[];
  revision: number;
  register(index: number, element: HTMLElement): void;
  find(index: number): HTMLElement | undefined;
}) {
  const shell = useShell();
  const [position, setPosition] = createSignal({ x: 300, y: 100 });
  const [size, setSize] = createSignal({ width: 520, height: 340 });
  const primary = () => props.surfaces[0] || { index: 0, rotation: 0, webrtc: false };
  const game = useMatch(() => "/game/:platform/*");
  const cursor = () => POINTER_PLATFORMS.has(game()?.params.platform ?? "") ? "hidden" : undefined;
  return <section class="horizon-surface-stage">
    <ClassicSurfaceBackdrop surface={() => props.find(primary().index)} revision={props.revision} />
    <tracked-surface ref={(element: HTMLElement) => props.register(primary().index, element)} src={String(primary().index)} data-index={String(primary().index)} class="horizon-game-surface" attr:data-cursor={cursor()} />
    <For each={props.surfaces.slice(1)}>{(surface) =>
      <FloatingPanel.Root open={shell.model.gamepadOpen} onOpenChange={(details: any) => shell.setModel("gamepadOpen", details.open)} position={position()} onPositionChange={(details: any) => setPosition(details.position)} size={size()} onSizeChange={(details: any) => setSize(details.size)} minSize={{ width: 240, height: 180 }} lockAspectRatio persistRect closeOnEscape={false}>
        <FloatingPanel.Positioner class="horizon-gamepad-positioner" data-open={shell.model.gamepadOpen} style={`position:fixed;top:0;left:0;width:${size().width}px;height:${size().height}px;transform:translate(${position().x}px,${position().y}px)`}>
          <FloatingPanel.Content class="horizon-gamepad-panel">
            <FloatingPanel.Header class="horizon-gamepad-header"><FloatingPanel.DragTrigger class="horizon-gamepad-drag">Gamepad display</FloatingPanel.DragTrigger><FloatingPanel.CloseTrigger class="horizon-gamepad-close" aria-label="Hide gamepad"><Icon name="close" tone="action" /></FloatingPanel.CloseTrigger></FloatingPanel.Header>
            <FloatingPanel.Body class="horizon-gamepad-body"><ClassicSurfaceBackdrop surface={() => props.find(surface.index)} revision={props.revision} /><tracked-surface ref={(element: HTMLElement) => props.register(surface.index, element)} src={String(surface.index)} data-index={String(surface.index)} class="horizon-game-surface" attr:data-cursor={cursor()} /></FloatingPanel.Body>
            <FloatingPanel.ResizeTrigger axis="se" class="horizon-gamepad-resize" aria-label="Resize gamepad display">⌟</FloatingPanel.ResizeTrigger>
          </FloatingPanel.Content>
        </FloatingPanel.Positioner>
      </FloatingPanel.Root>
    }</For>
    <Show when={props.surfaces.length === 1}>
      <tracked-surface ref={(element: HTMLElement) => props.register(primary().index === 1 ? 0 : 1, element)} src={String(primary().index === 1 ? 0 : 1)} class="horizon-spare-surface" />
    </Show>
  </section>;
}
