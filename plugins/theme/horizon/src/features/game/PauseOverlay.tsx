import { Dialog, Popover } from "@ark-ui/solid";
import { Show, createSignal } from "solid-js";
import { useShell } from "../../app/ShellProvider";
import { Icon } from "../../components/Icon";
import { VolumeSlider } from "../../components/FormControls";
import "./PauseOverlay.css";

export function PauseOverlay() {
  const shell = useShell();
  const [confirmClose, setConfirmClose] = createSignal(false);
  const [audioOpen, setAudioOpen] = createSignal(false);
  const resume = () => shell.setModel("pauseOpen", false);
  const setVolume = (value: number) => { shell.setModel("audio", "volume", value); shell.model.launch?.setVolume(value); };
  return <Show when={shell.model.pauseOpen}>
    <Dialog.Root open onOpenChange={(details: any) => shell.setModel("pauseOpen", details.open)} modal={false} closeOnEscape={false} preventScroll={false}>
      <Dialog.Backdrop class="horizon-pause-backdrop" />
      <Dialog.Positioner class="horizon-pause-positioner">
        <Dialog.Content class="horizon-pause-panel" onKeyDown={(event: KeyboardEvent) => {
        if (event.key !== "Escape") return;
        if (audioOpen()) { event.preventDefault(); event.stopPropagation(); setAudioOpen(false); }
        else if (confirmClose()) { event.preventDefault(); event.stopPropagation(); setConfirmClose(false); }
      }}>
        <Show when={confirmClose()} fallback={<>
          <Dialog.Title>Game paused</Dialog.Title>
          <Dialog.Description>Choose an option to continue.</Dialog.Description>
          <div class="horizon-pause-actions">
            <button class="primary" type="button" autofocus onClick={resume}>Resume</button>
            <button type="button" onClick={() => { shell.setModel("pauseOpen", false); shell.navigate("/game/remap"); }}><Icon name="gamepad" /> Controller mapping</button>
            <Show when={shell.model.surfaces.length > 1}><button type="button" onClick={() => shell.setModel("gamepadOpen", (open: boolean) => !open)}><Icon name="gamepad" /> {shell.model.gamepadOpen ? "Hide" : "Show"} gamepad display</button></Show>
            <Popover.Root open={audioOpen()} onOpenChange={(details: any) => setAudioOpen(details.open)} positioning={{ placement: "right" }}>
              <Popover.Trigger class="horizon-audio-trigger"><Icon name="volume" /> Audio</Popover.Trigger>
              <Popover.Positioner class="horizon-audio-positioner"><Popover.Content class="horizon-audio-popover"><Popover.Arrow><Popover.ArrowTip /></Popover.Arrow><Popover.Title>Game audio</Popover.Title><VolumeSlider value={shell.model.audio.volume} onChange={setVolume} /><button type="button" onClick={() => { const muted = !shell.model.audio.muted; shell.setModel("audio", "muted", muted); shell.model.launch?.setMuted(muted); }}>{shell.model.audio.muted ? "Unmute" : "Mute"}</button></Popover.Content></Popover.Positioner>
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
      </Dialog.Positioner>
    </Dialog.Root>
  </Show>;
}
