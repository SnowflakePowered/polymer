import { For, createEffect, createSignal, onCleanup, onMount } from "solid-js";
import type { RecentGame } from "../../app/models";
import { useShell } from "../../app/ShellProvider";
import { useInputActions } from "../../app/InputProvider";
import { hasActiveInputScope } from "../../components/InputNavigation";
import { GameTile } from "./GameTile";
import { gameId, nearestInDirection, type Direction } from "./spatial-navigation";
import "./GameWall.css";

export function GameWall(props: { games: RecentGame[] }) {
  const shell = useShell();
  const input = useInputActions();
  const [zoom, setZoom] = createSignal(1);
  const [controllerSelections, setControllerSelections] = createSignal<Record<string, string>>({});
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
  const playerIndex = (sourceId: string) => {
    const assigned = window.Snowglass.controllers.players.find((player) => player.controller?.id === sourceId);
    return assigned?.index ?? Math.max(0, window.Snowglass.controllers.connected.findIndex((source) => source.id === sourceId));
  };
  const playersFor = (id: string) => Object.entries(controllerSelections())
    .filter(([, game]) => game === id)
    .map(([sourceId]) => playerIndex(sourceId));
  const zoomWall = (event: WheelEvent) => {
    if (!event.ctrlKey || !event.deltaY) return;
    event.preventDefault();
    event.stopPropagation();
    const direction = event.deltaY < 0 ? -1 : 1;
    setZoom((value) => Math.min(1.8, Math.max(0.65, Math.round((value + direction * 0.1) * 10) / 10)));
  };

  onMount(() => {
    const unsubscribe = input.subscribe((action) => {
      if (hasActiveInputScope()) return;
      if (!props.games.length) return;
      const sourceId = action.sourceId;
      const current = sourceId ? controllerSelections()[sourceId] || gameId(props.games[0]) : selected() || gameId(props.games[0]);
      if (action.type === "move") {
        action.event.preventDefault();
        const next = nearestInDirection([...elements].map(([id, element]) => ({ id, rect: element.getBoundingClientRect() })), current, action.direction as Direction);
        if (next && sourceId) setControllerSelections((values) => ({ ...values, [sourceId]: next }));
        else if (next) choose(next);
        return;
      }
      if (action.type === "activate") {
        action.event.preventDefault();
        const game = gameFor(current); if (game) shell.launchRecent(game);
      } else if (action.type === "details") {
        action.event.preventDefault();
        const game = gameFor(current); if (game) shell.showDetails(game);
      }
    });
    onCleanup(unsubscribe);
  });

  return <section
    class="horizon-game-wall"
    aria-label="Recently played games"
    style={`--tile-min-height:${9 * zoom()}rem;--tile-fluid-height:${18 * zoom()}vh;--tile-max-height:${12 * zoom()}rem;--tile-min-width:${7 * zoom()}rem`}
    onWheel={zoomWall}
  >
    <For each={props.games}>{(game) => {
      const id = () => gameId(game);
      return <GameTile game={game} selected={selected() === id()} players={playersFor(id())} zoom={zoom()} register={(key, element) => elements.set(key, element)} onActivate={() => selected() === id() ? launch() : choose(id())} />;
    }}</For>
  </section>;
}
