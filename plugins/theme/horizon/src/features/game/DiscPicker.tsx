import { createSignal, onCleanup } from "solid-js";
import type { LaunchInstance } from "@polymer-fe/frontend";
import { ActionDialog, ActionRow, Icon } from "../../ui";

export function DiscPicker(props: { launch: LaunchInstance }) {
  const launch = props.launch;
  const discs = launch.getLaunchSet();
  const [current, setCurrent] = createSignal(launch.currentDiscIndex);
  const [choosing, setChoosing] = createSignal(false);
  const [pending, setPending] = createSignal(false);
  const [error, setError] = createSignal("");
  let row!: HTMLButtonElement;
  const discChanged = (event: Event) => setCurrent((event as CustomEvent<{ index: number }>).detail.index);
  launch.addEventListener("discchange", discChanged);
  onCleanup(() => launch.removeEventListener("discchange", discChanged));
  const change = (index: number) => {
    setPending(true);
    setError("");
    launch.changeDisc(index)
      .catch(reason => setError(reason instanceof Error ? reason.message : String(reason)))
      .finally(() => setPending(false));
  };
  return <>
    <ActionRow ref={row} disabled={!launch.canChangeDisc} pending={pending()} description={error() || `Playing disc ${current() + 1} of ${discs.length}`} onClick={() => setChoosing(true)}>
      <Icon name="disc" tone="action" /> Change disc
    </ActionRow>
    <ActionDialog open={choosing()} title="Change disc" returnFocus={() => row} onClose={() => setChoosing(false)} actions={discs.map((disc, index) => ({
      label: `Disc ${index + 1}`,
      description: index === current() ? "Currently playing" : disc.serial ?? undefined,
      disabled: index === current(),
      run: () => change(index),
    }))} />
  </>;
}
