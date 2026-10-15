import { Show, createMemo, createSignal } from "solid-js";
import { useShell } from "../../app/shell-context";
import { DialogParts as Dialog, OverlayPortal, ConfirmDialog, SettingRow, Toggle, ActionHints } from "../../ui";
import { VolumeSlider } from "./VolumeSlider";
import { PauseDecorator } from "./PauseDecorator";
import { ScreenLayoutPicker } from "./ScreenLayoutPicker";
import { ShaderPanel } from "./ShaderPicker";
import { SuspendPoints } from "./SuspendPoints";
import { DiscPicker } from "./DiscPicker";
import { DUAL_SCREEN_PLATFORMS } from "./DualScreenStage";
import { POINTER_PLATFORMS } from "./SurfaceStage";
import { useMatch } from "@solidjs/router";
import "./PauseOverlay.css";

type Panel = "options" | "load" | "save" | "shader";

export function PauseOverlay() {
  const shell = useShell();
  const [confirmClose, setConfirmClose] = createSignal(false);
  const game = createMemo(() => shell.model.libraryGames.find(entry => entry.path === shell.model.draft?.path));
  const resume = () => shell.setModel("pauseOpen", false);
  const dualScreen = useMatch(() => "/game/:platform/*", { platform: DUAL_SCREEN_PLATFORMS });
  const pointer = useMatch(() => "/game/:platform/*", { platform: [...POINTER_PLATFORMS] });
  const setVolume = (value: number) => { shell.setModel("audio", "volume", value); shell.model.launch?.setVolume(value); };
  const [panel, setPanel] = createSignal<Panel>("options");
  const tabs: Partial<Record<Panel, HTMLButtonElement>> = {};
  let menu!: HTMLElement, options!: HTMLElement;
  const back = () => {
    const tab = tabs[panel()];
    if (!tab || !options.contains(document.activeElement)) return false;
    tab.focus();
    return true;
  };
  const enter = (next: Panel) => { setPanel(next); requestAnimationFrame(() => options.querySelector<HTMLElement>(".hz-field > input, .horizon-suspend-slot:not(:disabled)")?.focus()); };
  const tab = (next: Panel, label: string) => <button ref={element => tabs[next] = element} type="button" class="horizon-pause-item" aria-selected={panel() === next} onFocus={() => setPanel(next)} onClick={() => enter(next)}>{label}</button>;
  return <Show when={shell.model.pauseOpen}><PauseDecorator>
    <Dialog.Root open closeOnEscape={false} onEscapeKeyDown={() => { if (!back()) resume(); }} initialFocusEl={() => menu.querySelector<HTMLButtonElement>("button")} onOpenChange={details => { if (!details.open) resume(); }}>
      <OverlayPortal>
        <Dialog.Backdrop class="horizon-pause-backdrop" />
        <Dialog.Positioner class="horizon-pause-shell">
          <Dialog.Content class="horizon-pause" onAction={action => {
            if ((action.type === "back" || (action.type === "move" && action.direction === "left")) && back()) return true;
            if (action.type !== "pause") return false;
            resume();
            return true;
          }}>
            <header class="horizon-pause-header">
              <Show when={game()?.icon}>{icon => <img src={icon()} alt="" />}</Show>
              <Dialog.Title class="horizon-pause-title">{game()?.name ?? shell.model.draft?.name ?? "Game paused"}</Dialog.Title>
            </header>
            <nav ref={menu} class="horizon-pause-menu" aria-label="Pause menu">
              <button type="button" class="horizon-pause-item" data-input-default onFocus={() => setPanel("options")} onClick={resume}>Continue</button>
              <Show when={shell.model.launch?.saveStates}>
                {tab("load", "Load Suspend Point")}
                {tab("save", "Create Suspend Point")}
              </Show>
              {tab("shader", "Shaders")}
              <button type="button" class="horizon-pause-item" onFocus={() => setPanel("options")} onClick={() => { resume(); shell.navigate(shell.gamePath("reorder")); }}>Change Controller Order</button>
              <button type="button" class="horizon-pause-item" onFocus={() => setPanel("options")} onClick={() => setConfirmClose(true)}>Close Game</button>
            </nav>
            <div class="horizon-pause-divider" aria-hidden="true" />
            <section ref={options} class="horizon-pause-options" data-nav-group aria-label={tabs[panel()]?.textContent ?? "Options"}>
              <Show when={panel() === "options"}>
                <SettingRow><VolumeSlider value={shell.model.audio.volume} onChange={setVolume} /></SettingRow>
                <Show when={(shell.model.launch?.getLaunchSet().length ?? 0) > 1 && shell.model.launch}>{launch => <DiscPicker launch={launch()} />}</Show>
                <ScreenLayoutPicker />
                <Show when={shell.model.surfaces.length > 1 && !dualScreen()}><SettingRow><Toggle label="Gamepad display" checked={shell.model.gamepadOpen} onCheckedChange={open => shell.setModel("gamepadOpen", open)} /></SettingRow></Show>
                <Show when={pointer()}><SettingRow><Toggle label="Show mouse cursor" checked={shell.model.cursorVisible} onCheckedChange={visible => shell.setModel("cursorVisible", visible)} /></SettingRow></Show>
              </Show>
              <Show when={panel() === "shader"}><ShaderPanel /></Show>
              <Show when={(panel() === "load" || panel() === "save") && shell.model.launch?.saveStates} keyed>{store => <SuspendPoints store={store} mode={panel() === "save" ? "save" : "load"} onRestored={resume} />}</Show>
            </section>
            <ActionHints floating />
          </Dialog.Content>
        </Dialog.Positioner>
      </OverlayPortal>
    </Dialog.Root>
    <ConfirmDialog open={confirmClose()} onClose={() => setConfirmClose(false)} title="Close this game?" description="Unsaved progress in the game will be lost." confirmLabel="Close game" onConfirm={shell.closeGame} />
  </PauseDecorator></Show>;
}
