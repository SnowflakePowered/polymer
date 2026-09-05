import { Dialog, Popover } from "@ark-ui/solid";
import { Show, createEffect, createSignal } from "solid-js";
import { useShell } from "../../app/shell-context";
import { Icon } from "../../components/Icon";
import { OverlayPortal } from "../../components/OverlayPortal";
import { VolumeSlider } from "../../components/FormControls";
import { PauseDecorator } from "./PauseDecorator";
import "./PauseOverlay.css";

export function PauseOverlay() {
  const shell = useShell();
  const [confirmClose, setConfirmClose] = createSignal(false);
  const [audioOpen, setAudioOpen] = createSignal(false);
  let panel: HTMLDivElement | undefined;
  let audioPanel: HTMLDivElement | undefined;
  const resume = () => shell.setModel("pauseOpen", false);
  const setVolume = (value: number) => { shell.setModel("audio", "volume", value); shell.model.launch?.setVolume(value); };
  createEffect(() => {
    const open = shell.model.pauseOpen;
    const confirming = confirmClose();
    const audio = audioOpen();
    if (!open) return;
    requestAnimationFrame(() => {
      const scope = audio ? audioPanel : panel;
      const preferred = scope?.querySelector<HTMLElement>("[autofocus]")
        || scope?.querySelector<HTMLElement>("button:not(:disabled), [tabindex]:not([tabindex='-1'])");
      if (preferred && (confirming || audio || !scope?.contains(document.activeElement))) preferred.focus();
    });
  });
  return <Show when={shell.model.pauseOpen}>
    <PauseDecorator><Dialog.Root open onOpenChange={(details: any) => shell.setModel("pauseOpen", details.open)} modal={false} closeOnEscape={false} preventScroll={false}>
      <OverlayPortal><Dialog.Backdrop class="horizon-pause-backdrop" />
        <Dialog.Positioner class="horizon-pause-positioner">
        <Dialog.Content ref={panel} class="horizon-pause-panel horizon-input-scope" onKeyDown={(event: KeyboardEvent) => {
        if (event.key !== "Escape") return;
        if (audioOpen()) { event.preventDefault(); event.stopPropagation(); setAudioOpen(false); }
        else if (confirmClose()) { event.preventDefault(); event.stopPropagation(); setConfirmClose(false); }
      }}>
        <Show when={confirmClose()} fallback={<>
          <Dialog.Title>Game paused</Dialog.Title>
          <Dialog.Description>Choose an option to continue.</Dialog.Description>
          <div class="horizon-pause-actions">
            <button class="primary" type="button" autofocus onClick={resume}>Resume</button>
            <button type="button" onClick={() => { shell.setModel("pauseOpen", false); shell.navigate("/game/reorder"); }}><Icon name="gamepad" /> Change controller order</button>
            <Show when={shell.model.surfaces.length > 1}><button type="button" onClick={() => shell.setModel("gamepadOpen", (open: boolean) => !open)}><Icon name="gamepad" /> {shell.model.gamepadOpen ? "Hide" : "Show"} gamepad display</button></Show>
            <Popover.Root open={audioOpen()} onOpenChange={(details: any) => setAudioOpen(details.open)} positioning={{ placement: "right" }}>
              <Popover.Trigger class="horizon-audio-trigger"><Icon name="volume" /> Audio</Popover.Trigger>
              <OverlayPortal><Popover.Positioner class="horizon-audio-positioner"><Popover.Content ref={audioPanel} class="horizon-audio-popover horizon-input-scope"><Popover.Arrow><Popover.ArrowTip /></Popover.Arrow><Popover.Title>Game audio</Popover.Title><VolumeSlider value={shell.model.audio.volume} onChange={setVolume} /><button type="button" autofocus onClick={() => { const muted = !shell.model.audio.muted; shell.setModel("audio", "muted", muted); shell.model.launch?.setMuted(muted); }}>{shell.model.audio.muted ? "Unmute" : "Mute"}</button></Popover.Content></Popover.Positioner></OverlayPortal>
            </Popover.Root>
            <button class="danger" type="button" onClick={() => setConfirmClose(true)}><Icon name="close" /> Close game</button>
          </div>
          <p class="horizon-pause-hint"><kbd>Esc</kbd> Resume</p>
        </>}>
          <Dialog.Title>Close this game?</Dialog.Title>
          <Dialog.Description>Unsaved progress in the game will be lost.</Dialog.Description>
          <div class="horizon-confirm-actions"><button type="button" autofocus onClick={() => setConfirmClose(false)}>Keep playing</button><button class="danger" type="button" onClick={shell.closeGame}>Close game</button></div>
        </Show>
        </Dialog.Content>
        </Dialog.Positioner></OverlayPortal>
    </Dialog.Root></PauseDecorator>
  </Show>;
}
