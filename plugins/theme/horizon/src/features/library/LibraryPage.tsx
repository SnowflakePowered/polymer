import { Show, createSignal, onCleanup, onMount } from "solid-js";
import { AppSidebar } from "../../components/AppSidebar";
import { ControlHintBar } from "../../components/ControlHintBar";
import { StatusBanner } from "../../components/StatusBanner";
import { useShell } from "../../app/ShellProvider";
import { EmptyLibrary } from "./EmptyLibrary";
import { GameWall } from "./GameWall";
import "./LibraryPage.css";

export function LibraryPage() {
  const shell = useShell();
  const [dropActive, setDropActive] = createSignal(window.Snowglass.scraping.dropActive);
  onMount(() => {
    const changed = (event: Event) => setDropActive((event as CustomEvent<boolean>).detail);
    window.Snowglass.scraping.addEventListener("dropactivechange", changed);
    onCleanup(() => window.Snowglass.scraping.removeEventListener("dropactivechange", changed));
  });
  const status = () => shell.model.launcher.kind === "detecting" ? `Preparing ${shell.model.launcher.name}…` : "";
  return <main class="horizon-library">
    <AppSidebar />
    <section class="horizon-library-content" data-drop-active={dropActive()}>
      <StatusBanner message={shell.model.error} tone="error" />
      <StatusBanner message={status()} />
      <Show when={shell.model.recent.length > 0} fallback={<EmptyLibrary dropActive={dropActive()} />}>
        <GameWall games={shell.model.recent} />
      </Show>
      <ControlHintBar hints={[{ key: "↑↓←→", label: "Move" }, { key: "Shift", label: "Details" }, { key: "Enter", label: "Start" }]} />
    </section>
  </main>;
}
