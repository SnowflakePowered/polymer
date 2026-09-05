import { Show, createSignal } from "solid-js";
import type { RecentGame } from "../../app/models";
import { friendly } from "../../app/launch";
import { gameId, tileColor, tileShape } from "./spatial-navigation";
import "./GameTile.css";

export function GameTile(props: {
  game: RecentGame;
  selected: boolean;
  players: number[];
  zoom: number;
  register(id: string, element: HTMLButtonElement): void;
  onActivate(): void;
}) {
  const [dimensions, setDimensions] = createSignal<{ url: string; ratio: number }>();
  const [failedArtwork, setFailedArtwork] = createSignal<string>();
  const artwork = () => props.game.artwork !== failedArtwork() ? props.game.artwork : undefined;
  const ratio = () => dimensions()?.url === artwork() ? dimensions()?.ratio : undefined;
  const id = () => gameId(props.game);
  const shape = () => tileShape(id(), props.zoom);
  return <button
    ref={(element) => props.register(id(), element)}
    type="button"
    class="horizon-game-tile"
    classList={{ selected: props.selected, "has-artwork-ratio": !!ratio() }}
    data-controller={props.players.length ? props.players[0] % 8 : undefined}
    style={`--tile-ratio:${ratio() ?? shape().ratio};--tile-basis:${shape().basis}px;--tile-grow:${shape().grow};--tile-color:${tileColor(id())};transform:scale(${props.selected || props.players.length ? 1.16 : 1})`}
    aria-label={`${props.game.name}, ${friendly(props.game.platform)}`}
    aria-current={props.selected ? "true" : undefined}
    onClick={props.onActivate}
  >
    <Show when={artwork()}>{url => <img class="horizon-game-artwork" src={url()} alt="" loading="lazy" decoding="async" onLoad={event => {
      const image = event.currentTarget;
      if (image.naturalWidth && image.naturalHeight) setDimensions({ url: url(), ratio: image.naturalWidth / image.naturalHeight });
    }} onError={() => setFailedArtwork(url())} />}</Show>
    <span class="horizon-game-title">{props.game.name}</span>
    <small>{friendly(props.game.platform)}</small>
  </button>;
}
