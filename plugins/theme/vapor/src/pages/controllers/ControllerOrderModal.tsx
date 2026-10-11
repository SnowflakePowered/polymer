import { For, Show, createEffect, createSignal, onCleanup, onMount } from "solid-js";
import { ControllerOrder } from "@polymer-fe/frontend/controller-order";
import type { ManagedControllerDevice } from "@polymer-fe/frontend";
import { useShell } from "../../app/shell";
import { Button, Icon, Modal, ProgressBar, Throbber } from "../../ui";
import "./controllers.css";

const NAVIGATION_KEYS = new Set(["Escape", "Enter", "Tab", " ", "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Shift", "PageUp", "PageDown"]);

function Battery(props: { device: ManagedControllerDevice }) {
  return <Show when={props.device.battery?.level != null ? props.device.battery : null}>{battery =>
    <span class="vp-order-battery"><ProgressBar value={battery().level! / 100} /><small>{battery().level}%{battery().charging ? " · Charging" : ""}</small></span>
  }</Show>;
}

function Order(props: { onDone(): void }) {
  const shell = useShell();
  const order = new ControllerOrder(window.Polymer.controllers, (shell.state.request ?? shell.state.launch?.request)?.controllerTopology.mappings ?? []);
  const [revision, setRevision] = createSignal(0);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const read = <T,>(value: () => T) => () => { revision(); return value(); };
  const slots = read(() => order.slots), candidates = read(() => order.candidates), ready = read(() => !!order.session);
  const availability = read(() => order.availability), discovering = read(() => order.discovering), lesser = read(() => order.lesserGroup);
  const at = (index: number) => { revision(); return order.controllerAt(index); };
  const fail = (reason: unknown) => setError(reason instanceof Error ? reason.message : String(reason));
  const done = async () => {
    if (busy()) return;
    setBusy(true);
    try { await order.commit(); props.onDone(); } catch (reason) { fail(reason); setBusy(false); }
  };
  onMount(() => {
    const changed = () => setRevision(value => value + 1);
    const failed = (event: Event) => fail((event as CustomEvent<unknown>).detail);
    const keyboard = (event: KeyboardEvent) => {
      if (NAVIGATION_KEYS.has(event.key) || event.repeat || !order.session || order.players.some(player => player.controller?.id === "keyboardMouse")) return;
      void order.join({ kind: "keyboardMouse" }).catch(fail);
    };
    order.addEventListener("change", changed);
    order.addEventListener("error", failed);
    window.addEventListener("keydown", keyboard);
    void order.start().catch(fail);
    onCleanup(() => {
      window.removeEventListener("keydown", keyboard);
      order.removeEventListener("change", changed);
      order.removeEventListener("error", failed);
      order.dispose();
    });
  });
  return <Modal open class="vp-order" onClose={() => void done()} title="Controller order" initialFocus={() => document.querySelector<HTMLElement>(".vp-order [data-autofocus]")}
    onAction={action => { if (action.type !== "activate" || document.activeElement?.closest(".vp-order-candidates, .vp-order-groups")) return false; void done(); return true; }}
    description="Press a button on each controller in the order players should join. Press any key to join with the keyboard."
    buttons={<Button primary data-autofocus disabled={busy() || !ready()} onClick={() => void done()}>{busy() ? "Applying…" : "Done"}</Button>}>
    <Show when={order.groups.length > 1}><div class="vp-order-groups vp-actions">
      <Button aria-pressed={lesser() === null} onClick={() => order.setLesserGroup(null)}>{order.groups[0][0].target.group}</Button>
      <For each={order.groups.slice(1)}>{group => <Button aria-pressed={lesser() === group[0].target.group} onClick={() => order.setLesserGroup(group[0].target.group)}>{group[0].target.group}</Button>}</For>
    </div></Show>
    <div class="vp-order-players" role="list" aria-label="Player order"><For each={slots()}>{(index, position) => <div class="vp-order-player" role="listitem" data-connected={!!at(index) || undefined}>
      <small>Player {position() + 1}</small>
      <Show when={at(index)} fallback={<span class="vp-order-empty"><Icon name="controller" size={40} color="#3d4450" /><span>Waiting</span></span>}>{device => <>
        <Icon name={device().id === "keyboardMouse" ? "key" : "controller"} size={40} />
        <strong title={device().name}>{device().name}</strong>
        <Battery device={device()} />
      </>}</Show>
    </div>}</For></div>
    <Show when={candidates().length}><h3 class="vp-section-title">Discovered controllers</h3><div class="vp-order-candidates"><For each={candidates()}>{candidate =>
      <div class="vp-field" data-icon=""><div class="vp-field-icon"><Icon name="controller" /></div><div class="vp-field-label">{candidate.name}</div>
        <div class="vp-field-children"><Show when={candidate.pairing} fallback={<Button disabled={busy()} onClick={() => void order.connect(candidate.id).catch(fail)}>Connect</Button>}><Throbber size="small" label="Pairing" /></Show></div>
      </div>
    }</For></div></Show>
    <div class="vp-order-status">
      <span data-unavailable={availability().osBle === "unavailable" || undefined}>Bluetooth</span>
      <span data-unavailable={availability().winUsb === "unavailable" || undefined}>USB receiver</span>
      <Show when={discovering()}><span class="vp-order-searching"><Throbber size="small" /> Searching</span></Show>
      <Show when={!ready() && !error()}><span>Preparing controllers…</span></Show>
    </div>
    <Show when={error()}><p class="vp-order-error" role="alert">{error()}</p></Show>
  </Modal>;
}

export function ControllerOrderModal() {
  const shell = useShell();
  const open = () => shell.state.menu === "order";
  createEffect(() => {
    const launch = shell.state.launch;
    if (!launch || !open()) return;
    const release = shell.pauses.acquire(launch);
    onCleanup(() => void release().catch(error => console.error("Failed to resume emulator", error)));
  });
  return <Show when={open()}><Order onDone={() => shell.update("menu", null)} /></Show>;
}
