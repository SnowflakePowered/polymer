import { Show, onCleanup, onMount } from "solid-js";
import { AppSidebar } from "../../components/AppSidebar";
import { ControlHintBar } from "../../components/ControlHintBar";
import { Icon } from "../../components/Icon";
import { useShell } from "../../app/ShellProvider";
import { cloneGame, friendly } from "../../app/launch";
import type { EmulatorConfiguration } from "../../../../../../js/snowglass";
import { ControllerSection } from "./ControllerSection";
import { SettingsSection } from "./SettingsSection";
import "./GameDetailsPage.css";

export function GameDetailsPage() {
  const shell = useShell();
  const game = () => shell.model.draft;
  const emulator = () => game()?.candidates.find((item) => item.id === game()?.selectedEmulator) || game()?.candidates[0];
  const updateEmulator = (change: (emulator: EmulatorConfiguration) => void) => {
    const current = game();
    if (!current) return;
    const next = cloneGame(current);
    const target = next.candidates.find((item) => item.id === next.selectedEmulator) || next.candidates[0];
    change(target);
    shell.setModel("draft", next);
  };
  onMount(() => {
    const keydown = (event: KeyboardEvent) => {
      if (event.defaultPrevented) return;
      if (event.key === "Escape") { event.preventDefault(); shell.cancelDetails(); }
      if (event.key === "Enter" && event.target === document.body) { event.preventDefault(); shell.launchDraft(); }
    };
    window.addEventListener("keydown", keydown);
    onCleanup(() => window.removeEventListener("keydown", keydown));
  });
  return <Show when={game() && emulator()} fallback={<main class="horizon-details-missing"><button onClick={shell.cancelDetails}>Return to library</button></main>}>
    <main class="horizon-details-shell">
      <AppSidebar />
      <section class="horizon-details-page">
        <header class="horizon-details-header">
          <button class="horizon-back-button" type="button" aria-label="Return to library" onClick={shell.cancelDetails}><Icon name="back" /></button>
          <div><p>{friendly(game()!.platform)}</p><h1>{game()!.name}</h1></div>
          <button class="horizon-start-button" type="button" onClick={() => shell.launchDraft()}>Start <Icon name="play" /></button>
        </header>
        <div class="horizon-details-columns">
          <ControllerSection game={game()!} emulator={emulator()!} update={updateEmulator} />
          <SettingsSection game={game()!} emulator={emulator()!} selectEmulator={(value) => shell.setModel("draft", "selectedEmulator", value)} update={updateEmulator} />
        </div>
        <ControlHintBar hints={[{ key: "Esc", label: "Back" }, { key: "Enter", label: "Start" }]} />
      </section>
    </main>
  </Show>;
}
