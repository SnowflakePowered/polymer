import { Show, createSignal, onCleanup, onMount } from "solid-js";
import { AppSidebar } from "../../components/AppSidebar";
import { ControlHintBar } from "../../components/ControlHintBar";
import { useShell } from "../../app/shell-context";
import { EmptyLibrary } from "./EmptyLibrary";
import { GameWall } from "./GameWall";
import "./LibraryPage.css";

export function LibraryPage() {
  const shell = useShell();
  const [dropActive, setDropActive] = createSignal(window.Polymer.scraping.dropActive);
  onMount(() => {
    const changed = (event: Event) => setDropActive((event as CustomEvent<boolean>).detail);
    window.Polymer.scraping.addEventListener("dropactivechange", changed);
    onCleanup(() => window.Polymer.scraping.removeEventListener("dropactivechange", changed));
  });
  const games = () => [...shell.model.libraryGames, ...shell.model.recent.filter(game => !shell.model.libraryGames.some(item => item.path === game.path))];
  return <main class="horizon-library">
    <AppSidebar />
    <section class="horizon-library-content" data-drop-active={dropActive()}>
      <Show when={games().length > 0} fallback={<EmptyLibrary dropActive={dropActive()} />}>
        <GameWall games={games()} />
      </Show>
      <ControlHintBar hints={[{ key: "↑↓←→", label: "Move" }, { key: "Shift", label: "Details" }, { key: "Enter", label: "Start" }]} />
    </section>
  </main>;
}
