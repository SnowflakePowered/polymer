import { For, Show, createEffect, createMemo, createSignal } from "solid-js";
import { createStore } from "solid-js/store";
import type { LibraryEntry, LibraryShowcase } from "@polymer-fe/frontend/library-state";
import { useShell } from "../../app/shell";
import { Capsule, CapsuleGrid, EmptyState, Icon, Page, Shelf } from "../../ui";
import { platformName } from "./platform";
import "./library.css";

export function HomePage() {
  const shell = useShell();
  const [focused, setFocused] = createSignal<string | null>(null);
  const recent = createMemo(() => {
    const known = new Map(shell.state.games.map(game => [game.path, game]));
    const played = shell.state.recent.map(entry => ({ ...entry, ...known.get(entry.path), playedAt: entry.playedAt }));
    return (played.length ? played : shell.state.games).slice(0, 12);
  });
  const rest = createMemo(() => shell.state.games.filter(game => !recent().some(entry => entry.path === game.path)));
  const current = () => recent().find(game => game.path === focused()) ?? recent()[0];
  const [showcases, setShowcases] = createStore<Record<string, LibraryShowcase>>({});
  createEffect(() => {
    for (const { path } of recent()) {
      if (path in showcases) continue;
      setShowcases(path, {});
      void shell.library.showcase(path).then(showcase => setShowcases(path, showcase)).catch(() => {});
    }
  });
  const background = (game?: LibraryEntry) => game ? showcases[game.path]?.background : undefined;
  const played = (game: LibraryEntry) => game.playedAt ? `Last played ${new Date(game.playedAt).toLocaleDateString()}` : platformName(game.platform);
  const open = (game: LibraryEntry) => void shell.open(game, "details");
  return <Page class="vp-home" focusWhen={() => recent().length} hints={[{ action: "activate", label: "Select" }]}>
    <Show when={recent().length} fallback={<EmptyState title="Your library is empty" body="Drop a game, folder or archive onto this window to add it." />}>
      <section class="vp-recent">
        <div class="vp-recent-backdrop"><Show when={background(current())}>{art => <img src={art()} alt="" />}</Show><span /></div>
        <Shelf title="Recent games">
          <For each={recent()}>{(game, index) =>
            <Capsule wide={index() === 0 && !!background(game)} title={game.name} subtitle={platformName(game.platform)} data-autofocus={index() === 0}
              art={index() === 0 && background(game) || game.artwork} logo={index() === 0 && background(game) ? showcases[game.path]?.logo : undefined} ratio={game.artworkRatio}
              onFocus={() => setFocused(game.path)} onClick={() => open(game)} />
          }</For>
        </Shelf>
        <Show when={current()}>{game => <div class="vp-recent-label"><strong>{game().name}</strong><span><Icon name="play" size={12} color="#59bf40" />{played(game())}</span></div>}</Show>
      </section>
      <Show when={rest().length}><h2 class="vp-shelf-title">Library</h2><CapsuleGrid><For each={rest()}>{game =>
        <Capsule title={game.name} subtitle={platformName(game.platform)} art={game.artwork} ratio={game.artworkRatio} badge={game.missingKey ? "Missing key" : undefined}
          progress={game.scraping ? game.scraping.completed / Math.max(1, game.scraping.total) : undefined} onClick={() => open(game)} />
      }</For></CapsuleGrid></Show>
    </Show>
  </Page>;
}
