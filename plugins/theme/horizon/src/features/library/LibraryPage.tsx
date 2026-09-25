import { Show, createSignal, onCleanup, onMount, type ParentProps } from "solid-js";
import { useLocation } from "@solidjs/router";
import { AppSidebar } from "../../components/AppSidebar";
import { ActionHints } from "../../ui";
import { useShell } from "../../app/shell-context";
import { gameId } from "./spatial-navigation";
import { css } from "../../../styled-system/css";
import { EmptyLibrary } from "./EmptyLibrary";
import { GameWall } from "./GameWall";
import "./LibraryPage.css";

const toolbar = css({
  display: "flex", flex: "none", alignItems: "center", gap: "2rem", height: "4rem", marginInline: "1.5rem", paddingInline: ".5rem",
  borderTop: "1px solid token(colors.divider)",
  "& .hz-hints": { flex: "1", minWidth: "0", height: "100%", padding: "0", borderTop: "0" },
});
const selection = css({ display: "flex", minWidth: "0", flex: "0 1 auto", alignItems: "baseline", gap: ".75rem", overflow: "hidden", whiteSpace: "nowrap" });
const selectionTitle = css({ overflow: "hidden", textOverflow: "ellipsis", fontSize: "1.25rem", fontWeight: "500" });
const selectionPlatform = css({ flex: "none", color: "muted", fontSize: ".95rem" });

export function LibraryPage(props: ParentProps) {
  const shell = useShell();
  const location = useLocation();
  const settingsOpen = () => location.pathname === "/details";
  const [dropActive, setDropActive] = createSignal(window.Polymer.scraping.dropActive);
  onMount(() => {
    const changed = (event: Event) => setDropActive((event as CustomEvent<boolean>).detail);
    window.Polymer.scraping.addEventListener("dropactivechange", changed);
    onCleanup(() => window.Polymer.scraping.removeEventListener("dropactivechange", changed));
  });
  const games = () => [...shell.model.libraryGames, ...shell.model.recent.filter(game => !shell.model.libraryGames.some(item => item.path === game.path))];
  const selectedGame = () => games().find(game => gameId(game) === shell.model.selectedGame) ?? games()[0];
  return <><main class="horizon-library" inert={settingsOpen()} aria-hidden={settingsOpen() ? "true" : undefined}>
    <AppSidebar />
    <section class="horizon-library-content" data-drop-active={dropActive()}>
      <Show when={games().length > 0} fallback={<EmptyLibrary dropActive={dropActive()} />}>
        <GameWall games={games()} />
      </Show>
      <footer class={toolbar}>
        <Show when={selectedGame()}>{game => <div class={selection}><span class={selectionTitle}>{game().name}</span><span class={selectionPlatform}>{window.Polymer.machine(game().platform)?.name ?? game().platform}</span></div>}</Show>
        <ActionHints />
      </footer>
    </section>
  </main>{props.children}</>;
}
