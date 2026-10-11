import { Show, batch, createEffect, createMemo, createSignal, onCleanup, type ParentProps } from "solid-js";
import { unwrap } from "solid-js/store";
import { useMatch } from "@solidjs/router";
import type { LaunchInstance } from "@polymer-fe/frontend";
import { GameLauncher } from "@polymer-fe/frontend/launch";
import { useShell } from "../../app/shell-context";
import { DUAL_SCREEN_LAYOUTS, DUAL_SCREEN_PLATFORMS, DualScreenStage, type DualScreenLayout } from "./DualScreenStage";
import { SurfaceStage } from "./SurfaceStage";
import "./GameLayout.css";

export function GameLayout(props: ParentProps) {
  const shell = useShell();
  const [revision, setRevision] = createSignal(0);
  const games = new GameLauncher(window.Polymer);
  const elements = new Map<number, HTMLElement>();
  const surfaces = createMemo(() => shell.model.surfaces.length ? shell.model.surfaces : [{ index: 0, rotation: 0, webrtc: false }]);
  const dualScreen = useMatch(() => "/game/:platform/*", { platform: DUAL_SCREEN_PLATFORMS });
  const layoutRoute = useMatch(() => "/game/:platform/:game/play/:layout", { layout: [...DUAL_SCREEN_LAYOUTS] });
  const layout = () => (layoutRoute()?.params.layout ?? "stacked") as DualScreenLayout;
  const register = (index: number, element: HTMLElement) => {
    element.setAttribute("src", String(index));
    element.setAttribute("data-index", String(index));
    elements.set(index, element);
  };

  let sequence = 0;
  let boundId = -1;
  let unbind = () => {};
  onCleanup(() => { sequence++; unbind(); });

  const finish = (message?: string) => {
    batch(() => {
      if (message) shell.setModel("error", message);
      shell.setModel("request", null);
      shell.setModel("launch", null);
      shell.setModel("surfaces", []);
      shell.setModel("pauseOpen", false);
      shell.setModel("gamepadOpen", false);
      shell.setModel("launcher", { kind: "idle" });
    });
    shell.navigate("/", true);
  };
  const bind = (instance: LaunchInstance) => {
    if (boundId === instance.id) return;
    unbind();
    boundId = instance.id;
    unbind = games.watch(instance, {
      ready: () => { shell.setModel("launcher", { kind: "idle" }); shell.setModel("error", ""); shell.rememberGame(); shell.navigate(shell.gamePath("play"), true); },
      audio: state => shell.setModel("audio", state),
      surfaces: () => setRevision((value) => value + 1),
      finished: finish,
    });
  };

  createEffect(() => { if (shell.model.launch) bind(shell.model.launch); });
  createEffect(() => {
    const request = shell.model.request;
    const currentSequence = ++sequence;
    if (!request || shell.model.launch) return;
    // Launch off the unwrapped request: store proxies over nested values
    // such as `resolution` do not survive the op boundary as arrays.
    games.start(unwrap(request), index => elements.get(index), () => currentSequence !== sequence || shell.model.request !== request).then(instance => {
      if (!instance) return;
      shell.setModel("launch", instance);
      bind(instance);
    }, error => {
      shell.setModel("request", null);
      shell.setModel("launcher", { kind: "idle" });
      shell.setModel("error", error instanceof Error ? error.message : String(error));
      shell.navigate("/", true);
    });
  });

  return <main class="horizon-game-layout">
    <Show when={dualScreen()} fallback={<SurfaceStage surfaces={surfaces()} revision={revision()} register={register} find={(index) => elements.get(index)} />}>
      <DualScreenStage layout={layout()} register={register} />
    </Show>
    {props.children}
  </main>;
}
