import { For, Show, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import type { InputSource, InputSourceAssignment } from "../../../../../js/snowglass";
import { ChoiceControl } from "./FormControls";
import { Icon } from "./Icon";
import "./ControllerAssignmentPanel.css";

function selectorId(assignment: InputSourceAssignment) {
  if (assignment.source.kind === "keyboardMouse") return "keyboard-mouse";
  if (assignment.source.kind === "device") return assignment.source.deviceId;
  return "";
}

export function ControllerAssignmentPanel(props: { onClose?: () => void }) {
  const [revision, setRevision] = createSignal(0);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  const invalidate = () => setRevision((value) => value + 1);
  onMount(() => {
    window.Snowglass.input.addEventListener("configurationchange", invalidate);
    window.Snowglass.input.addEventListener("inputstatechange", invalidate);
    onCleanup(() => {
      window.Snowglass.input.removeEventListener("configurationchange", invalidate);
      window.Snowglass.input.removeEventListener("inputstatechange", invalidate);
    });
  });
  const assignments = () => (revision(), window.Snowglass.input.assignments);
  const sources = () => (revision(), window.Snowglass.input.sources);
  const slotCount = createMemo(() => {
    const highestPort = Math.max(-1, ...window.Snowglass.input.ports.map((port) => port.slot ?? -1));
    const highestAssignment = Math.max(-1, ...assignments().map((assignment) => assignment.index));
    return Math.max(4, highestPort + 1, highestAssignment + 1, window.Snowglass.input.controllers.length);
  });
  const sourceAt = (index: number) => {
    const assignment = assignments().find((item) => item.index === index);
    return assignment ? selectorId(assignment) : "";
  };
  const sourceDetails = (id: string): InputSource | undefined => sources().find((source) => source.id === id);
  const options = () => [
    { value: "", label: "Not assigned" },
    { value: "keyboard-mouse", label: "Keyboard + mouse" },
    ...sources().filter((source) => source.kind === "controller").map((source) => ({
      value: source.id,
      label: `${source.name}${source.connected ? "" : " (disconnected)"}`,
    })),
  ];
  const assign = async (index: number, sourceId: string) => {
    if (busy()) return;
    setBusy(true);
    setError("");
    const next = assignments()
      .map((assignment) => ({ index: assignment.index, sourceId: selectorId(assignment) }))
      .filter((assignment) => assignment.sourceId && assignment.index !== index && assignment.sourceId !== sourceId);
    if (sourceId) next.push({ index, sourceId });
    next.sort((left, right) => left.index - right.index);
    try {
      await window.Snowglass.input.assign(next);
      invalidate();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  };

  return <section class="horizon-assignment-panel" aria-busy={busy()}>
    <header>
      <div><p>Change grip / order</p><h2>Assign controllers</h2></div>
      <Show when={props.onClose}><button type="button" onClick={props.onClose} aria-label="Close controller assignments"><Icon name="close" /></button></Show>
    </header>
    <p class="horizon-assignment-help">Choose which real input source controls each virtual player. Disconnected assignments are retained until you change them.</p>
    <div class="horizon-assignment-cards">
      <For each={Array.from({ length: slotCount() }, (_, index) => index)}>{(index) => {
        const selected = () => sourceAt(index);
        const source = () => sourceDetails(selected());
        return <article class="horizon-assignment-card" data-player={index % 8} data-connected={source()?.connected || selected() === "keyboard-mouse"}>
          <div class="horizon-player-number">{index + 1}</div>
          <div class="horizon-assignment-device">
            <Icon name={selected() === "keyboard-mouse" ? "keyboard" : "gamepad"} />
            <strong>{source()?.name || (selected() === "keyboard-mouse" ? "Keyboard + mouse" : "Open")}</strong>
            <span>{source()?.connected === false ? "Disconnected · assignment retained" : selected() ? "Ready" : "Choose an input source"}</span>
          </div>
          <ChoiceControl label={`Player ${index + 1} source`} value={selected()} items={options()} onChange={(value) => void assign(index, value)} />
        </article>;
      }}</For>
    </div>
    <Show when={error()}><p class="horizon-assignment-error" role="alert">{error()}</p></Show>
  </section>;
}
