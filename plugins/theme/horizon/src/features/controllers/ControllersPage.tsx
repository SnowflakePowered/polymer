import { For, Show, createSignal, onCleanup, onMount } from "solid-js";
import type { ManagedControllerDevice } from "@polymer-fe/frontend";
import { css } from "../../../styled-system/css";
import { ActionRow, ControllerIllustration, Icon, Screen, ScreenHeader } from "../../ui";
import { AppSidebar } from "../../components/AppSidebar";
import { useShell } from "../../app/shell-context";
import { ControllerReorder } from "../game/ControllerReorder";

const PLAYER_LIGHTS = ["1000", "1100", "1110", "1111", "1001", "1010", "1011", "0110"];

const shellLayout = css({ display: "flex", height: "100%", color: "text", bg: "surface" });
const screen = css({ minWidth: "0", flex: "1" });
const headerIcon = css({ width: "2.4rem", height: "2.4rem", flex: "none" });
const layout = css({ display: "grid", gridTemplateColumns: "minmax(0, 1fr) 16rem", gap: "1.5rem", alignItems: "start" });
const grid = css({ display: "grid", gridTemplateColumns: "repeat(4, minmax(0, 1fr))", gap: "1.5rem 1rem", minHeight: "12rem", alignContent: "start" });
const slot = css({ display: "flex", flexDirection: "column", alignItems: "center", gap: ".5rem", minWidth: "0", textAlign: "center", "& .hz-controller-illustration": { width: "6rem", height: "4.5rem" } });
const lights = css({ display: "flex", gap: ".25rem", "& i": { width: ".55rem", height: ".55rem", bg: "track" }, "& i[data-lit]": { bg: "#3fd61c" } });
const name = css({ maxWidth: "100%", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", fontSize: ".85em", color: "muted" });
const battery = css({ display: "inline-flex", alignItems: "center", gap: ".25rem", fontSize: ".8em", color: "muted", "& > span": { position: "relative", width: "1.6rem", height: ".8rem", border: "2px solid currentColor", borderRadius: ".15rem", overflow: "hidden" }, "& > span > i": { position: "absolute", inset: "1px", right: "auto", bg: "text" }, "&[data-low] > span > i": { bg: "#f06a1d" }, "&[data-charging] > span > i": { bg: "#3fd61c" } });
const empty = css({ gridColumn: "1 / -1", color: "muted", margin: "0" });
const menu = css({ display: "flex", flexDirection: "column", gap: ".5rem", paddingLeft: "1.5rem", borderLeft: "1px solid token(colors.divider)", "& .hz-action-row": { padding: "1rem 1.5rem", borderRadius: "4px" } });

function BatteryMeter(props: { device: ManagedControllerDevice }) {
  return <Show when={props.device.battery?.level != null ? props.device.battery : null}>{state => <span class={battery} data-low={state().level! <= 20 || undefined} data-charging={state().charging || undefined} aria-label={`Battery ${state().level}%${state().charging ? ", charging" : ""}`}>
    <span><i style={{ width: `calc(${state().level}% - 2px)` }} /></span><Show when={state().charging}>⚡</Show>
  </span>}</Show>;
}

export function ControllersPage() {
  const shell = useShell();
  const [reordering, setReordering] = createSignal(false);
  const [revision, setRevision] = createSignal(0);
  const players = () => {
    revision();
    return window.Polymer.controllers.players.filter(player => player.controller);
  };

  onMount(() => {
    const changed = () => setRevision(value => value + 1);
    window.Polymer.controllers.addEventListener("statechange", changed);
    window.Polymer.controllers.addEventListener("playerchange", changed);
    onCleanup(() => {
      window.Polymer.controllers.removeEventListener("statechange", changed);
      window.Polymer.controllers.removeEventListener("playerchange", changed);
    });
  });

  return <div class={shellLayout}><AppSidebar /><Screen class={screen} hints={[{ action: "back", label: "Back" }, { action: "activate", label: "OK" }]} onAction={action => { if (action.type !== "back") return false; shell.navigate("/"); return true; }}>
    <ScreenHeader title="Controllers" icon={<Icon name="gamepad" class={headerIcon} strokeWidth={1.4} />} />
    <div class={layout}>
      <section class={grid} aria-label="Connected controllers">
        <For each={players()} fallback={<p class={empty}>No controllers are connected.</p>}>{player => <div class={slot}>
          <span class={lights} aria-label={`Player ${player.index + 1}`}><For each={[...PLAYER_LIGHTS[player.index % PLAYER_LIGHTS.length]]}>{lit => <i data-lit={lit === "1" || undefined} />}</For></span>
          <ControllerIllustration kind={{ LeftHalf: "left", RightHalf: "right", Whole: "pro" }[player.controller!.role] as "left" | "right" | "pro"} label={player.controller!.name} />
          <span class={name} title={player.controller!.name}>{player.controller!.name}</span>
          <BatteryMeter device={player.controller!} />
        </div>}</For>
      </section>
      <nav class={menu} aria-label="Controller actions">
        <ActionRow data-input-default onClick={() => setReordering(true)}>Change Grip/Order</ActionRow>
      </nav>
    </div>
    <Show when={reordering()}><ControllerReorder onClose={() => setReordering(false)} /></Show>
  </Screen></div>;
}
