import { For, Show, createEffect, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import type { RecentGame } from "../../app/models";
import { useShell } from "../../app/shell-context";
import { useInputActions } from "../../app/InputProvider";
import { hasActiveInputScope } from "../../components/InputNavigation";
import { CoverLayout } from "./cover-layout";
import { GameTile } from "./GameTile";
import { GameContextMenu } from "./GameContextMenu";
import { gameId, nearestInDirection, type Direction } from "./spatial-navigation";
import "./GameWall.css";

export function GameWall(props: { games: RecentGame[] }) {
  const shell = useShell();
  const input = useInputActions();
  const [context, setContext] = createSignal<{ game: RecentGame }>();
  const [zoom, setZoom] = createSignal(1);
  const [viewport, setViewport] = createSignal({ width: 1, height: 800 });
  const [ratios, setRatios] = createSignal<Record<string, number>>({});
  let wall!: HTMLElement;
  const layout = createMemo(() => CoverLayout.arrange(
    props.games.map(game => ratios()[game.artwork || gameId(game)] ?? 0.72),
    viewport().width, Math.min(240, Math.max(144, viewport().height * .3)) * zoom(),
  ));
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
    const assigned = window.Polymer.controllers.players.find((player) => player.controller?.id === sourceId);
    return assigned?.index ?? Math.max(0, window.Polymer.controllers.connected.findIndex((source) => source.id === sourceId));
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
    const observer = new ResizeObserver(([entry]) => setViewport({ width: Math.max(1, entry.contentRect.width), height: entry.contentRect.height }));
    observer.observe(wall);
    onCleanup(() => observer.disconnect());
    const unsubscribe = input.subscribe((action) => {
      if (hasActiveInputScope()) return;
      const focused = document.activeElement;
      if (action.origin === "keyboard" && focused instanceof HTMLElement
        && (focused.closest(".horizon-sidebar") || focused.closest(".horizon-toast-region"))) return;
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
    ref={wall}
    class="horizon-game-wall"
    aria-label="Game library"
    onWheel={zoomWall}
  >
    <div class="horizon-game-wall-canvas" style={{ height: `${layout().height}px` }}>
    <For each={props.games}>{(game, index) => {
      const id = () => gameId(game);
      return <GameTile game={game} selected={selected() === id()} players={playersFor(id())} placement={layout().placements[index()]} onRatio={ratio => setRatios(values => ({ ...values, [game.artwork || id()]: ratio }))} register={(key, element) => element ? elements.set(key, element) : elements.delete(key)} onActivate={() => selected() === id() ? launch() : choose(id())} onContextMenu={() => { shell.setModel("selectedGame", id()); setContext({ game }); }} />;
    }}</For>
    </div>
    <Show when={context()}>{value => <GameContextMenu {...value()} onClose={() => setContext(undefined)} />}</Show>
  </section>;
}
