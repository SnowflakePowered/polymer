import { createEffect, createSignal, onCleanup } from "solid-js";
import { CoverArt } from "../../components/CoverArt";
import type { RecentGame } from "../../app/models";
import { friendly } from "../../app/launch";
import type { CoverPlacement } from "./cover-layout";
import { gameId, tileColor } from "./spatial-navigation";
import "./GameTile.css";

export function GameTile(props: {
  game: RecentGame;
  selected: boolean;
  players: number[];
  placement: CoverPlacement;
  onRatio(ratio: number): void;
  register(id: string, element: HTMLButtonElement | null): void;
  onActivate(): void;
}) {
  const [ready, setReady] = createSignal(false);
  createEffect(() => { props.game.artwork; setReady(false); });
  const id = () => gameId(props.game);
  onCleanup(() => props.register(id(), null));

  return <button
    ref={(element) => props.register(id(), element)}
    type="button"
    class="horizon-game-tile"
    classList={{ selected: props.selected, "has-artwork-ratio": ready() }}
    data-controller={props.players.length ? props.players[0] % 8 : undefined}
    style={{ left: `${props.placement.x}px`, top: `${props.placement.y}px`, width: `${props.placement.width}px`, height: `${props.placement.height}px`, "--tile-color": tileColor(id()), transform: `scale(${props.selected || props.players.length ? 1.06 : 1})` }}
    aria-label={`${props.game.name}, ${friendly(props.game.platform)}`}
    aria-current={props.selected ? "true" : undefined}
    onClick={props.onActivate}
  >
    <CoverArt src={props.game.artwork} onRatio={props.onRatio} onReady={setReady} />
    <span class="horizon-game-title">{props.game.name}</span>
    <small>{friendly(props.game.platform)}</small>
  </button>;
}
