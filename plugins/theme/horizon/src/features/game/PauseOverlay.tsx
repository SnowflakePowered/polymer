import { Show, createMemo, createSignal } from "solid-js";
import { useShell } from "../../app/shell-context";
import { SidePanel, ConfirmDialog, ActionRow, SettingRow, Toggle, Icon, ActionHints } from "../../ui";
import { VolumeSlider } from "./VolumeSlider";
import { PauseDecorator } from "./PauseDecorator";
import { ScreenLayoutPicker } from "./ScreenLayoutPicker";
import { ShaderPicker } from "./ShaderPicker";
import { DiscPicker } from "./DiscPicker";
import { DUAL_SCREEN_PLATFORMS } from "./DualScreenStage";
import { useMatch } from "@solidjs/router";
import "./PauseOverlay.css";

export function PauseOverlay() {
  const shell = useShell();
  const [confirmClose, setConfirmClose] = createSignal(false);
  const game = createMemo(() => shell.model.libraryGames.find(entry => entry.path === shell.model.draft?.path));
  const resume = () => shell.setModel("pauseOpen", false);
  const dualScreen = useMatch(() => "/game/:platform/*", { platform: DUAL_SCREEN_PLATFORMS });
  const setVolume = (value: number) => { shell.setModel("audio", "volume", value); shell.model.launch?.setVolume(value); };
  const title = <span class="horizon-quick-menu-title"><Show when={game()?.icon}>{icon => <img src={icon()} alt="" />}</Show><span>{game()?.name ?? shell.model.draft?.name ?? "Game paused"}</span></span>;
  return <Show when={shell.model.pauseOpen}><PauseDecorator>
    <SidePanel open class="horizon-quick-menu" onAction={action => { if (action.type !== "pause") return false; resume(); return true; }} onOpenChange={open => { if (!open) resume(); }} title={title} actions={<ActionHints />}>
      <ActionRow autofocus onClick={resume}><Icon name="play" tone="action" /> Resume</ActionRow>
      <SettingRow><VolumeSlider value={shell.model.audio.volume} onChange={setVolume} /></SettingRow>
      <Show when={(shell.model.launch?.getLaunchSet().length ?? 0) > 1 && shell.model.launch}>{launch => <DiscPicker launch={launch()} />}</Show>
      <ScreenLayoutPicker />
      <ShaderPicker />
      <Show when={shell.model.surfaces.length > 1 && !dualScreen()}><SettingRow><Toggle label="Gamepad display" checked={shell.model.gamepadOpen} onCheckedChange={open => shell.setModel("gamepadOpen", open)} /></SettingRow></Show>
      <ActionRow onClick={() => { resume(); shell.navigate(shell.gamePath("reorder")); }}><Icon name="gamepad" tone="action" /> Change controller order</ActionRow>
      <ActionRow appearance="danger" onClick={() => setConfirmClose(true)}><Icon name="close" tone="danger" /> Close game</ActionRow>
    </SidePanel>
    <ConfirmDialog open={confirmClose()} onClose={() => setConfirmClose(false)} title="Close this game?" description="Unsaved progress in the game will be lost." confirmLabel="Close game" onConfirm={shell.closeGame} />
  </PauseDecorator></Show>;
}
