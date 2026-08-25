import type { RecentGame } from "../../app/models";
import { friendly } from "../../app/launch";
import { gameId, tileColor, tileShape } from "./spatial-navigation";
import "./GameTile.css";

export function GameTile(props: {
  game: RecentGame;
  selected: boolean;
  zoom: number;
  register(id: string, element: HTMLButtonElement): void;
  onActivate(): void;
}) {
  const id = () => gameId(props.game);
  const shape = () => tileShape(id(), props.zoom);
  return <button
    ref={(element) => props.register(id(), element)}
    type="button"
    class="horizon-game-tile"
    classList={{ selected: props.selected }}
    style={`--tile-basis:${shape().basis}px;--tile-grow:${shape().grow};--tile-color:${tileColor(id())};transform:scale(${props.selected ? 1.16 : 1})`}
    aria-label={`${props.game.name}, ${friendly(props.game.platform)}`}
    aria-current={props.selected ? "true" : undefined}
    onClick={props.onActivate}
  >
    <span class="horizon-game-title">{props.game.name}</span>
    <small>{friendly(props.game.platform)}</small>
  </button>;
}
