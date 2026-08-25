import { For, createEffect, createSignal, onCleanup, onMount } from "solid-js";
import type { RecentGame } from "../../app/models";
import { useShell } from "../../app/ShellProvider";
import { GameTile } from "./GameTile";
import { gameId, nearestInDirection, type Direction } from "./spatial-navigation";
import "./GameWall.css";

export function GameWall(props: { games: RecentGame[] }) {
  const shell = useShell();
  const [zoom, setZoom] = createSignal(1);
  const elements = new Map<string, HTMLButtonElement>();
  const selected = () => shell.model.selectedGame || (props.games[0] ? gameId(props.games[0]) : null);
  const gameFor = (id: string | null) => props.games.find((game) => gameId(game) === id);

  createEffect(() => {
    if (!shell.model.selectedGame && props.games[0]) shell.setModel("selectedGame", gameId(props.games[0]));
    if (shell.model.selectedGame && !gameFor(shell.model.selectedGame) && props.games[0]) shell.setModel("selectedGame", gameId(props.games[0]));
  });

  const choose = (id: string) => {
    shell.setModel("selectedGame", id);
    requestAnimationFrame(() => elements.get(id)?.focus());
  };
  const launch = () => { const game = gameFor(selected()); if (game) shell.launchRecent(game); };
  const details = () => { const game = gameFor(selected()); if (game) shell.showDetails(game); };
  const zoomWall = (event: WheelEvent) => {
    if (!event.ctrlKey || !event.deltaY) return;
    event.preventDefault();
    event.stopPropagation();
    const direction = event.deltaY < 0 ? -1 : 1;
    setZoom((value) => Math.min(1.8, Math.max(0.65, Math.round((value + direction * 0.1) * 10) / 10)));
  };

  onMount(() => {
    const keydown = (event: KeyboardEvent) => {
      const directions: Partial<Record<string, Direction>> = { ArrowLeft: "left", ArrowRight: "right", ArrowUp: "up", ArrowDown: "down" };
      const direction = directions[event.key];
      if (direction) {
        event.preventDefault();
        const next = nearestInDirection([...elements].map(([id, element]) => ({ id, rect: element.getBoundingClientRect() })), selected() || "", direction);
        if (next) choose(next);
        return;
      }
      if (event.key === "Enter" && !event.repeat) { event.preventDefault(); launch(); }
      if ((event.code === "ShiftLeft" || event.code === "ShiftRight") && !event.repeat) { event.preventDefault(); details(); }
    };
    window.addEventListener("keydown", keydown);
    onCleanup(() => window.removeEventListener("keydown", keydown));
  });

  return <section
    class="horizon-game-wall"
    aria-label="Recently played games"
    style={`--tile-min-height:${9 * zoom()}rem;--tile-fluid-height:${18 * zoom()}vh;--tile-max-height:${12 * zoom()}rem;--tile-min-width:${7 * zoom()}rem`}
    onWheel={zoomWall}
  >
    <For each={props.games}>{(game) => {
      const id = () => gameId(game);
      return <GameTile game={game} selected={selected() === id()} zoom={zoom()} register={(key, element) => elements.set(key, element)} onActivate={() => selected() === id() ? launch() : choose(id())} />;
    }}</For>
  </section>;
}
