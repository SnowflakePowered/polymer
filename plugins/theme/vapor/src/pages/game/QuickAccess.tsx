import { Show, createEffect, createSignal, onCleanup } from "solid-js";
import { useShell } from "../../app/shell";
import { Button, ConfirmModal, DropdownField, PanelSection, SidePanel, SliderField } from "../../ui";

export function QuickAccess() {
  const shell = useShell();
  const [confirm, setConfirm] = createSignal(false);
  const open = () => shell.state.menu === "quick" && !!shell.state.launch;
  createEffect(() => {
    const launch = shell.state.launch;
    if (!launch || !open()) return;
    const release = shell.pauses.acquire(launch);
    onCleanup(() => void release().catch(error => console.error("Failed to resume emulator", error)));
  });
  const discs = () => shell.state.launch?.getLaunchSet() ?? [];
  const close = () => shell.update("menu", null);
  return <>
    <SidePanel side="right" open={open()} onClose={close} class="vp-quick-access" onAction={action => { if (action.type !== "pause") return false; close(); return true; }}>
      <h2 class="vp-quick-access-title">{shell.state.draft?.name ?? "Now playing"}</h2>
      <PanelSection title="Game">
        <Button primary data-autofocus onClick={close}>Resume game</Button>
        <Button onClick={() => shell.update("menu", "order")}>Controller order</Button>
        <Button danger onClick={() => setConfirm(true)}>Exit game</Button>
      </PanelSection>
      <PanelSection title="Audio">
        <SliderField label="Volume" bottomSeparator="none" value={Math.round(shell.state.audio.volume * 100)} step={5} format={value => `${value}%`} onChange={value => shell.setVolume(value / 100)} />
      </PanelSection>
      <Show when={discs().length > 1 && shell.state.launch?.canChangeDisc}>
        <PanelSection title="Disc">
          <DropdownField label="Current disc" bottomSeparator="none" value={String(shell.state.launch!.currentDiscIndex)} options={discs().map((_, index) => ({ value: String(index), label: `Disc ${index + 1}` }))}
            onChange={value => void shell.state.launch?.changeDisc(Number(value)).catch(shell.fail)} />
        </PanelSection>
      </Show>
    </SidePanel>
    <ConfirmModal open={confirm()} onClose={() => setConfirm(false)} title="Exit game?" description="Unsaved progress will be lost." confirm="Exit game" danger onConfirm={() => shell.state.launch?.close()} />
  </>;
}
