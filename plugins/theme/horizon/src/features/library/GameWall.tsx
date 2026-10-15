import { For, Show, createEffect, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import type { RecentGame } from "../../app/models";
import { useShell } from "../../app/shell-context";
import { useInputScope, useNavigation } from "../../ui";
import { CoverLayout } from "./cover-layout";
import { GameTile } from "./GameTile";
import { GameContextMenu } from "./GameContextMenu";
import { GameInfoCard } from "./GameInfoCard";
import { gameId, nearestInDirection, type Direction } from "./spatial-navigation";
import "./GameWall.css";

export function GameWall(props: { games: RecentGame[]; onEnd?: () => void; onTopEdge?: () => void }) {
  const shell = useShell();
  const navigation = useNavigation();
  const [context, setContext] = createSignal<{ game: RecentGame }>();
  const [info, setInfo] = createSignal<RecentGame>();
  const [zoom, setZoom] = createSignal(1);
  const [viewport, setViewport] = createSignal({ width: 1, height: 800 });
  const [ratios, setRatios] = createSignal<Record<string, number>>({});
  let wall!: HTMLElement;
  const layout = createMemo(() => CoverLayout.arrange(
    props.games.map(game => ratios()[game.artwork || gameId(game)] ?? game.artworkRatio ?? 0.72),
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

  createEffect(() => {
    props.games;
    requestAnimationFrame(() => { if (document.activeElement === document.body || !document.activeElement?.isConnected) elements.get(selected() || "")?.focus(); });
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
    const observer = new ResizeObserver(([entry]) => setViewport({ width: Math.max(1, entry.contentRect.width), height: wall.parentElement!.clientHeight - wall.offsetHeight + entry.contentRect.height }));
    observer.observe(wall);
    onCleanup(() => observer.disconnect());
    const keydown = (event: KeyboardEvent) => {
      const focused = [...elements].find(([, element]) => element === event.target)?.[0];
      const game = event.key === "Tab" && !event.shiftKey && !navigation.hasOverlay() && gameFor(focused ?? null);
      if (!game) return;
      event.preventDefault();
      setInfo(game);
    };
    wall.addEventListener("keydown", keydown);
    onCleanup(() => wall.removeEventListener("keydown", keydown));
    requestAnimationFrame(() => { if ((document.activeElement === document.body || document.activeElement === document.documentElement)) elements.get(selected() || "")?.focus(); });
  });
  useInputScope(() => ({ root: wall, navigation: true,
    hints: () => navigation.origin() === "keyboard"
      ? [{ action: "move", label: "Move" }, { action: "details", label: "Details", keyboard: "tab" }, { action: "details", label: "Settings" }, { action: "activate", label: "Search", keyboard: "space" }, { action: "activate", label: "Start" }]
      : [{ action: "move", label: "Move" }, { action: "details", label: "Settings" }, { action: "activate", label: "Start" }],
    handle: action => {
      if (!props.games.length) return false;
      const sourceId = action.sourceId;
      const focused = [...elements].find(([, element]) => element === document.activeElement)?.[0];
      const current = sourceId ? controllerSelections()[sourceId] || gameId(props.games[0]) : focused || selected() || gameId(props.games[0]);
      if (action.type === "previous" || action.type === "next") {
        const entries = [...elements].sort((a, b) => a[1].offsetTop - b[1].offsetTop || a[1].offsetLeft - b[1].offsetLeft);
        const index = entries.findIndex(([id]) => id === current);
        const page = Math.max(1, entries.filter(([, element]) => Math.abs(element.offsetTop - (elements.get(current)?.offsetTop ?? 0)) < wall.clientHeight).length);
        const next = entries[Math.max(0, Math.min(entries.length - 1, index + (action.type === "previous" ? -page : page)))]?.[0];
        if (next) { if (sourceId) setControllerSelections(values => ({ ...values, [sourceId]: next })); else choose(next); elements.get(next)?.scrollIntoView?.({ block: "nearest" }); }
        return true;
      }
      if (action.type === "move") {
        const next = nearestInDirection([...elements].map(([id, element]) => ({ id, rect: element.getBoundingClientRect() })), current, action.direction as Direction);
        if (!next && action.direction === "up" && !sourceId && props.onTopEdge) { props.onTopEdge(); return true; }
        if (!next) return false;
        if (sourceId) setControllerSelections(values => ({ ...values, [sourceId]: next }));
        else choose(next);
        elements.get(next)?.scrollIntoView?.({ block: "nearest", inline: "nearest" });
        return true;
      }
      if (action.type === "activate" && action.origin === "keyboard" && (action.event as KeyboardEvent).key === " ") { shell.setModel("librarySearchOpen", true); return true; }
      if (action.type === "activate") { const game = gameFor(current); if (game) shell.launchRecent(game); return true; }
      if (action.type === "details") { const game = gameFor(current); if (game) shell.showDetails(game); return true; }
      return false;
    },
  }));

  return <section
    ref={wall}
    class="horizon-game-wall"
    aria-label="Game library"
    onWheel={zoomWall}
    onScroll={event => { const element = event.currentTarget; if (element.scrollTop + element.clientHeight * 2 >= element.scrollHeight) props.onEnd?.(); }}
  >
    <div class="horizon-game-wall-canvas" style={{ height: `${layout().height}px` }}>
    <For each={props.games}>{(game, index) => {
      const id = () => gameId(game);
      return <GameTile game={game} closing={shell.model.closingGame === game.path} selected={selected() === id()} players={playersFor(id())} placement={layout().placements[index()]} onRatio={ratio => setRatios(values => ({ ...values, [game.artwork || id()]: ratio }))} register={(key, element) => element ? elements.set(key, element) : elements.delete(key)} onActivate={() => selected() === id() ? launch() : choose(id())} onContextMenu={() => { shell.setModel("selectedGame", id()); setContext({ game }); }} />;
    }}</For>
    </div>
    <Show when={context()}>{value => <GameContextMenu {...value()} onClose={() => setContext(undefined)} />}</Show>
    <Show when={info()} keyed>{game => <GameInfoCard game={game} returnFocus={() => elements.get(gameId(game))} onClose={() => setInfo(undefined)} />}</Show>
  </section>;
}
