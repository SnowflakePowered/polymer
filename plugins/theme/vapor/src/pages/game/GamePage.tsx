import { For, Show, createEffect, createMemo, onCleanup } from "solid-js";
import { unwrap } from "solid-js/store";
import type { LaunchInstance } from "../../../../../../js/polymer";
import { useShell } from "../../app/shell";
import { Throbber, useInputScope } from "../../ui";
import { QuickAccess } from "./QuickAccess";
import "./game.css";

export function GamePage() {
  const shell = useShell();
  const elements = new Map<number, HTMLElement>();
  const surfaces = createMemo(() => shell.state.request?.surfaces ?? shell.state.launch?.request.surfaces ?? []);
  let sequence = 0, bound = -1, unbind = () => {};
  onCleanup(() => { sequence++; unbind(); });

  const bind = (instance: LaunchInstance) => {
    if (bound === instance.id) return;
    unbind();
    bound = instance.id;
    unbind = shell.games.watch(instance, {
      ready: () => shell.started(instance),
      audio: state => shell.update("audio", state),
      surfaces: () => {},
      finished: error => shell.finished(error),
    });
  };
  createEffect(() => { if (shell.state.launch) bind(shell.state.launch); });
  createEffect(() => {
    const request = shell.state.request, current = ++sequence;
    if (!request || shell.state.launch) return;
    shell.games.start(unwrap(request), index => elements.get(index), () => current !== sequence || shell.state.request !== request)
      .then(instance => { if (instance) { shell.update("launch", instance); bind(instance); } }, error => shell.finished(error instanceof Error ? error.message : String(error)));
  });

  useInputScope(() => ({ root: document.body, handle: action => {
    if (!shell.state.launch || action.type !== "pause" && !(action.type === "back" && action.origin === "keyboard")) return false;
    shell.update("menu", (menu: string | null) => menu === "quick" ? null : "quick");
    return true;
  } }));

  return <main class="vp-game">
    <For each={surfaces().length ? surfaces() : [{ index: 0 }]}>{surface =>
      <tracked-surface ref={(element: HTMLElement) => { element.setAttribute("src", String(surface.index)); elements.set(surface.index, element); }}
        src={String(surface.index)} class="vp-game-surface" data-index={String(surface.index)} />
    }</For>
    <Show when={shell.state.phase.kind !== "idle"}>
      <section class="vp-launching" aria-live="polite"><Throbber /><p>{shell.state.phase.kind === "preparing" ? "Preparing" : "Launching"} {"name" in shell.state.phase ? shell.state.phase.name : ""}</p></section>
    </Show>
    <QuickAccess />
  </main>;
}
