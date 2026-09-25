import { Show, createSignal } from "solid-js";
import { useShell } from "../../app/shell-context";
import { Dialog, ConfirmDialog, ActionRow, Popover, OverlayPortal, Icon, Button, ActionHints } from "../../ui";
import { VolumeSlider } from "./VolumeSlider";
import { PauseDecorator } from "./PauseDecorator";

export function PauseOverlay() {
  const shell = useShell();
  const [confirmClose, setConfirmClose] = createSignal(false);
  const [audioOpen, setAudioOpen] = createSignal(false);
  const resume = () => shell.setModel("pauseOpen", false);
  const setVolume = (value: number) => { shell.setModel("audio", "volume", value); shell.model.launch?.setVolume(value); };
  return <Show when={shell.model.pauseOpen}><PauseDecorator>
    <Dialog open onAction={action => { if (action.type !== "pause") return false; resume(); return true; }} onOpenChange={open => { if (!open) resume(); }} title="Game paused" description="Choose an option to continue.">
      <ActionRow appearance="primary" autofocus onClick={resume}>Resume</ActionRow>
      <ActionRow onClick={() => { resume(); shell.navigate("/game/reorder"); }}><Icon name="gamepad" tone="action" /> Change controller order</ActionRow>
      <Show when={shell.model.surfaces.length > 1}><ActionRow onClick={() => shell.setModel("gamepadOpen", (open: boolean) => !open)}><Icon name="gamepad" tone="action" /> {shell.model.gamepadOpen ? "Hide" : "Show"} gamepad display</ActionRow></Show>
      <Popover.Root open={audioOpen()} onOpenChange={details => setAudioOpen(details.open)} positioning={{ placement: "right" }}>
        <Popover.Trigger class="hz-action-row hz-button"><Icon name="volume" tone="action" /> Audio</Popover.Trigger>
        <OverlayPortal><Popover.Positioner class="hz-picker-positioner"><Popover.Content>
          <Popover.Title>Game audio</Popover.Title><VolumeSlider value={shell.model.audio.volume} onChange={setVolume} />
          <Button onClick={() => { const muted = !shell.model.audio.muted; shell.setModel("audio", "muted", muted); shell.model.launch?.setMuted(muted); }}>{shell.model.audio.muted ? "Unmute" : "Mute"}</Button>
        </Popover.Content></Popover.Positioner></OverlayPortal>
      </Popover.Root>
      <ActionRow appearance="danger" onClick={() => setConfirmClose(true)}><Icon name="close" tone="danger" /> Close game</ActionRow>
      <ActionHints />
    </Dialog>
    <ConfirmDialog open={confirmClose()} onClose={() => setConfirmClose(false)} title="Close this game?" description="Unsaved progress in the game will be lost." confirmLabel="Close game" onConfirm={shell.closeGame} />
  </PauseDecorator></Show>;
}
