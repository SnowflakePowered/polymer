import { DialogParts as Dialog, Button, ButtonGlyph, ActionHints } from "../../ui";
import { Show } from "solid-js";
import { produce, unwrap } from "solid-js/store";
import { Icon } from "../../ui";
import { OverlayPortal } from "../../ui";
import { useShell } from "../../app/shell-context";
import type { EmulatorConfiguration } from "../../../../../../js/polymer";
import { SettingsSection } from "./SettingsSection";
import "./GameDetailsPage.css";

export function GameDetailsPage() {
  const shell = useShell();
  let panel: HTMLDivElement | undefined;
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
    const launchableId = shell.model.launchableId;
    const current = emulator();
    if (!launchableId || !current) return;
    const values = structuredClone(unwrap(current.userConfiguration)) as Record<string, Record<string, boolean | number | string | null>>;
    (values[group] ||= {})[option] = value;
    updateEmulator((candidate) => {
      candidate.userConfiguration = values;
    });
    void window.Polymer.configuration.configure(launchableId, current.id, values)
      .then((result) => {
        if (!result) return;
        updateEmulatorById(current.id, (candidate) => {
          candidate.configuration = result.configuration;
          candidate.userConfiguration = result.userConfiguration;
        });
      })
      .catch((error) => shell.setModel("error", error instanceof Error ? error.message : String(error)));
  };
  return <Show when={game() && emulator()} fallback={<main class="horizon-details-missing"><Button onClick={shell.cancelDetails}>Return to library</Button></main>}>
    <Dialog.Root open initialFocusEl={() => panel?.querySelector<HTMLButtonElement>(".hz-categories button") ?? null} onOpenChange={(details) => { if (!details.open) shell.cancelDetails(); }}>
      <OverlayPortal>
      <Dialog.Backdrop class="horizon-details-backdrop" />
      <Dialog.Positioner class="horizon-details-shell">
      <Dialog.Content ref={panel} class="horizon-details-page horizon-input-scope">
        <header class="horizon-details-header">
          <Icon name="settings" strokeWidth={1.4} />
          <Dialog.Title class="horizon-details-title">Game Settings</Dialog.Title>
          <p>{game()!.name} <span>· {window.Polymer.machine(game()!.platform)?.name ?? game()!.platform}</span></p>
        </header>
        <SettingsSection game={game()!} emulator={emulator()!} selectEmulator={(value) => shell.setModel("draft", "selectedEmulator", value)} updateEmulator={updateEmulator} updateConfiguration={updateConfiguration} />
        <div class="horizon-details-footer">
          <Button class="horizon-details-start" type="button" onClick={() => shell.launchDraft()}><Icon name="play" tone="action" />Start game</Button>
          <Button type="button" title="Escape" onClick={shell.cancelDetails}>Back</Button>
          <ActionHints />
        </div>
      </Dialog.Content>
      </Dialog.Positioner>
      </OverlayPortal>
    </Dialog.Root>
  </Show>;
}
