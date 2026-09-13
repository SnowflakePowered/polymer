import { batch, createEffect, createMemo, createSignal, onCleanup, type ParentProps } from "solid-js";
import { unwrap } from "solid-js/store";
import type { LaunchInstance, LaunchOptions } from "../../../../../../js/polymer";
import { useShell } from "../../app/shell-context";
import { SurfaceStage } from "./SurfaceStage";
import "./GameLayout.css";

export function GameLayout(props: ParentProps) {
  const shell = useShell();
  const [revision, setRevision] = createSignal(0);
  const elements = new Map<number, HTMLElement>();
  const surfaces = createMemo(() => shell.model.surfaces.length ? shell.model.surfaces : [{ index: 0, rotation: 0, webrtc: false }]);
  const register = (index: number, element: HTMLElement) => {
    element.setAttribute("src", String(index));
    element.setAttribute("data-index", String(index));
    elements.set(index, element);
  };

  let sequence = 0;
  let boundId = -1;
  let unbind = () => {};
  onCleanup(() => { sequence++; unbind(); });

  const bind = (instance: LaunchInstance) => {
    if (boundId === instance.id) return;
    unbind();
    boundId = instance.id;
    void instance.ready.catch(() => {});
    const ready = () => { shell.setModel("launcher", { kind: "idle" }); shell.setModel("error", ""); shell.rememberGame(); shell.navigate("/game/play", true); };
    const audio = (event: Event) => shell.setModel("audio", (event as CustomEvent<{ available: boolean; volume: number; muted: boolean }>).detail);
    const surfacesChanged = () => setRevision((value) => value + 1);
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
    const failed = (event: Event) => {
      const error = (event as CustomEvent<Error>).detail;
      finish(error?.message || "Game launch failed");
    };
    const closed = () => finish();
    instance.addEventListener("ready", ready);
    instance.addEventListener("audioChanged", audio);
    instance.addEventListener("surfacesChanged", surfacesChanged);
    instance.addEventListener("error", failed);
    instance.addEventListener("closed", closed);
    unbind = () => {
      instance.removeEventListener("ready", ready);
      instance.removeEventListener("audioChanged", audio);
      instance.removeEventListener("surfacesChanged", surfacesChanged);
      instance.removeEventListener("error", failed);
      instance.removeEventListener("closed", closed);
    };
    if (instance.state === "ready") ready();
    else if (instance.state === "error") failed(new CustomEvent("error", { detail: instance.error }));
    else if (instance.state === "closed") closed();
  };

  createEffect(() => { if (shell.model.launch) bind(shell.model.launch); });
  createEffect(() => {
    const request = shell.model.request;
    const currentSequence = ++sequence;
    if (!request || shell.model.launch) return;
    const wait = (remaining: number) => requestAnimationFrame(() => {
      if (currentSequence !== sequence || shell.model.request !== request) return;
      try {
        // Launch off the unwrapped request: store proxies over nested values
        // such as `resolution` do not survive the op boundary as arrays.
        const plan = unwrap(request);
        const attached = plan.surfaces.map((surface) => {
          const attach = elements.get(surface.index);
          if (!attach?.isConnected || attach.localName !== "tracked-surface") throw new Error(`Surface ${surface.index} is not ready`);
          return { ...surface, attach };
        });
        const instance = window.Polymer.emulator.launch({ ...plan, surfaces: attached } as LaunchOptions);
        shell.setModel("launch", instance);
        bind(instance);
      } catch (error) {
        if (remaining > 0) { wait(remaining - 1); return; }
        shell.setModel("request", null);
        shell.setModel("launcher", { kind: "idle" });
        shell.setModel("error", error instanceof Error ? error.message : String(error));
        shell.navigate("/", true);
      }
    });
    wait(180);
  });

  return <main class="horizon-game-layout"><SurfaceStage surfaces={surfaces()} revision={revision()} register={register} find={(index) => elements.get(index)} />{props.children}</main>;
}
