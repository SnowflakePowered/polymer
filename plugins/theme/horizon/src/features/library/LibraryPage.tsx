import { Show, createSignal, onCleanup, onMount, type ParentProps } from "solid-js";
import { useLocation } from "@solidjs/router";
import { AppSidebar } from "../../components/AppSidebar";
import { EmptyState } from "../../ui";
import { useShell } from "../../app/shell-context";
import { EmptyLibrary } from "./EmptyLibrary";
import { GameWall } from "./GameWall";
import { LibrarySearch } from "./LibrarySearch";
import { PlatformShelf } from "./PlatformShelf";
import "./LibraryPage.css";

export function LibraryPage(props: ParentProps) {
  const shell = useShell();
  const location = useLocation();
  const settingsOpen = () => location.pathname === "/details";
  const [dropActive, setDropActive] = createSignal(window.Polymer.scraping.dropActive);
  const [shelfOpen, setShelfOpen] = createSignal(false);
  const closeShelf = () => {
    setShelfOpen(false);
    requestAnimationFrame(() => document.activeElement?.isConnected && document.activeElement !== document.body || (document.querySelector<HTMLElement>(".horizon-game-tile[aria-current='true']") ?? document.querySelector<HTMLElement>(".horizon-game-tile"))?.focus());
  };
  onMount(() => {
    const changed = (event: Event) => setDropActive((event as CustomEvent<boolean>).detail);
    window.Polymer.scraping.addEventListener("dropactivechange", changed);
    onCleanup(() => window.Polymer.scraping.removeEventListener("dropactivechange", changed));
  });
  const filtered = () => shell.model.libraryFiltered;
  const games = () => filtered() ? shell.model.libraryGames : [...shell.model.libraryGames, ...shell.model.recent.filter(game => !shell.model.libraryGames.some(item => item.path === game.path))];
  return <><main class="horizon-library" inert={settingsOpen()} aria-hidden={settingsOpen() ? "true" : undefined}>
    <AppSidebar />
    <section class="horizon-library-content" data-drop-active={dropActive()}>
      <Show when={shell.model.librarySearchOpen}><LibrarySearch /></Show>
      <Show when={shelfOpen()}><PlatformShelf onExit={closeShelf} /></Show>
      <Show when={games().length > 0} fallback={<Show when={filtered()} fallback={<EmptyLibrary dropActive={dropActive()} />}><EmptyState title="No games match these filters." /></Show>}>
        <GameWall games={games()} onEnd={shell.loadMoreGames} onTopEdge={() => setShelfOpen(true)} />
      </Show>
    </section>
  </main>{props.children}</>;
}
