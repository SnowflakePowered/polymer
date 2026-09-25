import { Match, Switch, createSignal } from "solid-js";
import { ControllerOverview } from "../features/game/ControllerOverview";
import { ControllerReorder } from "../features/game/ControllerReorder";
import { SidebarButton } from "./SidebarButton";

export function ControllerStatus() {
  const [view, setView] = createSignal<"closed" | "overview" | "reorder">("closed");
  return <>
    <SidebarButton icon="gamepad" iconSize={42} label="Controllers" active={view() !== "closed"} expanded={view() !== "closed"} onClick={() => setView("overview")} />
    <Switch>
      <Match when={view() === "overview"}><ControllerOverview onChangeOrder={() => setView("reorder")} onClose={() => setView("closed")} /></Match>
      <Match when={view() === "reorder"}><ControllerReorder onClose={() => setView("overview")} /></Match>
    </Switch>
  </>;
}
