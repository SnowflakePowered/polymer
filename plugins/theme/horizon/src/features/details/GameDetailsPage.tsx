import { Show, onCleanup, onMount } from "solid-js";
import { produce, unwrap } from "solid-js/store";
import { AppSidebar } from "../../components/AppSidebar";
import { ControlHintBar } from "../../components/ControlHintBar";
import { Icon } from "../../components/Icon";
import { useShell } from "../../app/ShellProvider";
import { friendly } from "../../app/launch";
import type { EmulatorConfiguration } from "../../../../../../js/snowglass";
import { ControllerSection } from "./ControllerSection";
import { SettingsSection } from "./SettingsSection";
import "./GameDetailsPage.css";

export function GameDetailsPage() {
  const shell = useShell();
  const game = () => shell.model.draft;
  const emulator = () => game()?.candidates.find((item) => item.id === game()?.selectedEmulator) || game()?.candidates[0];
  const updateEmulatorById = (id: string, change: (emulator: EmulatorConfiguration) => void) => {
    const current = game();
    if (!current) return;
    const index = current.candidates.findIndex((item) => item.id === id);
    if (index < 0) return;
    shell.setModel("draft", "candidates", index, produce(change));
  };
  const updateEmulator = (change: (emulator: EmulatorConfiguration) => void) => {
    const current = emulator();
    if (current) updateEmulatorById(current.id, change);
  };
  const updateConfiguration = (group: string, option: string, value: boolean | number | string | null) => {
    const packageValue = shell.model.package;
    const current = emulator();
    if (!packageValue || !current) return;
    const values = structuredClone(unwrap(current.userConfiguration)) as Record<string, Record<string, boolean | number | string | null>>;
    (values[group] ||= {})[option] = value;
    updateEmulator((candidate) => {
      candidate.userConfiguration = values;
    });
    void window.Snowglass.configuration.configure(packageValue, current.id, values)
      .then((result) => {
        if (!result) return;
        updateEmulatorById(current.id, (candidate) => {
          candidate.configuration = result.configuration;
          candidate.userConfiguration = result.userConfiguration;
        });
      })
      .catch((error) => shell.setModel("error", error instanceof Error ? error.message : String(error)));
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
          <SettingsSection game={game()!} emulator={emulator()!} selectEmulator={(value) => shell.setModel("draft", "selectedEmulator", value)} updateConfiguration={updateConfiguration} />
        </div>
        <ControlHintBar hints={[{ key: "Esc", label: "Back" }, { key: "Enter", label: "Start" }]} />
      </section>
    </main>
  </Show>;
}
