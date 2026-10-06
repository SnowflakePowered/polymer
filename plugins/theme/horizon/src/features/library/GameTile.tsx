import "../../components/CoverArt.css";
import { For, Show, createEffect, createSignal, onCleanup } from "solid-js";
import { CoverArt, Tile, ScrollingText, PlayerBadge, Icon } from "../../ui";
import { Progress as ProgressIndicator } from "../../ui";
import type { RecentGame } from "../../app/models";
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
  onContextMenu(event: MouseEvent): void;
}) {
  const [focused, setFocused] = createSignal(false);
  const [ready, setReady] = createSignal(false);
  createEffect(() => { props.game.artwork; setReady(false); });
  const id = () => gameId(props.game);
  onCleanup(() => props.register(id(), null));

  return <Tile
    ref={(element) => props.register(id(), element)}
    type="button"
    class="horizon-game-tile"
    data-select-ring
    classList={{ "has-artwork-ratio": ready() || !!props.game.artworkBlurhash, scraping: !!props.game.scraping }}
    data-controller={props.players.length ? props.players[0] % 8 : undefined}
    style={{ left: `${props.placement.x}px`, top: `${props.placement.y}px`, width: `${props.placement.width}px`, height: `${props.placement.height}px`, "--tile-color": tileColor(id()) }}
    aria-label={`${props.game.name}, ${window.Polymer.machine(props.game.platform)?.name ?? props.game.platform}${props.game.missingKey ? ", missing key" : ""}`}
    aria-current={props.selected ? "true" : undefined}
    aria-haspopup="dialog"
    onFocus={() => setFocused(true)} onBlur={() => setFocused(false)}
    onClick={props.onActivate}
    onContextMenu={event => { event.preventDefault(); event.currentTarget.focus(); props.onContextMenu(event); }}
  >
    <CoverArt class="horizon-cover-art" src={props.game.artwork} blurhash={props.game.artworkBlurhash} onRatio={props.onRatio} onReady={setReady} />
    <span class="horizon-game-title"><ScrollingText active={focused()}>{props.game.name}</ScrollingText></span>
    <small>{window.Polymer.machine(props.game.platform)?.name ?? props.game.platform}</small><span class="horizon-game-players"><For each={props.players}>{player => <PlayerBadge player={player} />}</For></span>
    <Show when={props.game.missingKey}>{key => <span class="horizon-game-missing-key" title={`Missing key: ${key().id}`} role="img" aria-label="Missing key">
      <Icon name="key" tone="onBrand" strokeWidth={2.2} />
    </span>}</Show>
    <Show when={props.game.scraping}>{progress => <div class="horizon-game-scraping">
      <ProgressIndicator label={`Scraping ${props.game.name}`} completed={progress().completed} total={progress().total} />
    </div>}</Show>
  </Tile>;
}
