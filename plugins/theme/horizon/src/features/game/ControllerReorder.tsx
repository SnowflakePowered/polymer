import { Dialog } from "@ark-ui/solid";
import { For, Show, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import type { ConnectionSession, ManagedControllerDevice } from "../../../../../../js/snowglass";
import buttonA from "../../assets/switch/button-a.svg";
import buttonB from "../../assets/switch/button-b.svg";
import buttonL from "../../assets/switch/button-l.svg";
import buttonMinus from "../../assets/switch/button-minus.svg";
import buttonPlus from "../../assets/switch/button-plus.svg";
import buttonR from "../../assets/switch/button-r.svg";
import controllerPro from "../../assets/switch/controller-pro.svg";
import controllerSwitch from "../../assets/switch/controller-switch.svg";
import joyconLeft from "../../assets/switch/joycon-left-horizontal.svg";
import joyconRight from "../../assets/switch/joycon-right-horizontal.svg";
import { useShell } from "../../app/ShellProvider";
import { OverlayPortal } from "../../components/OverlayPortal";
import { PauseDecorator, type PauseRelease } from "./PauseDecorator";
import "./ControllerReorder.css";

const DISCOVERED_CONTROLLER_LIMIT = 8;

export function ControllerReorder(props: { onClose?: () => void }) {
  const shell = useShell();
  const [session, setSession] = createSignal<ConnectionSession | null>(null);
  const [revision, setRevision] = createSignal(0);
  const [pending, setPending] = createSignal(false);
  const [configuring, setConfiguring] = createSignal(false);
  const [error, setError] = createSignal("");
  let leaving = false;
  let disposed = false;
  let releasePause: PauseRelease | undefined;
  let content: HTMLDivElement | undefined;

  const players = createMemo(() => {
    revision();
    return session()?.players ?? window.Snowglass.controllers.players;
  });
  const candidates = createMemo(() => {
    revision();
    const assigned = new Set(players().flatMap((player) => player.controller ? [player.controller.id] : []));
    return (session()?.candidates ?? [])
      .filter((candidate) => !candidate.connected && !assigned.has(candidate.id))
      .slice(0, DISCOVERED_CONTROLLER_LIMIT);
  });
  const availability = () => {
    revision();
    return window.Snowglass.controllers.availability;
  };
  const discoveryActive = () => {
    revision();
    return session()?.discovery.active ?? false;
  };
  const isPairing = (candidate: ManagedControllerDevice) => candidate.pairing === true;

  const closeOverlay = () => {
    if (props.onClose) props.onClose();
    else {
      shell.setModel("pauseOpen", false);
      shell.navigate("/game/play", true);
    }
  };

  const commit = async () => {
    if (leaving) return;
    leaving = true;
    setPending(true);
    setConfiguring(true);
    try {
      await new Promise<void>((resolve) => {
        requestAnimationFrame(() => setTimeout(resolve, 0));
      });
      if (session()?.active) await session()!.commit();
      await releasePause?.();
      closeOverlay();
    } catch (reason) {
      leaving = false;
      setPending(false);
      setConfiguring(false);
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  };

  const connect = async (candidate: ManagedControllerDevice) => {
    setPending(true);
    setError("");
    try {
      await session()?.connect(candidate.id);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setPending(false);
    }
  };

  onMount(() => {
    const changed = () => setRevision((value) => value + 1);
    window.Snowglass.controllers.addEventListener("statechange", changed);
    void (async () => {
      try {
        const active = window.Snowglass.controllers.activeConnectionSession
          ?? await window.Snowglass.controllers.startConnectionSession();
        if (disposed) {
          if (active.active) await active.commit();
          return;
        }
        setSession(active);
        active.addEventListener("change", changed);
        setRevision((value) => value + 1);
        requestAnimationFrame(() => content?.querySelector<HTMLElement>(".horizon-order-ready")?.focus());
      } catch (reason) {
        if (!disposed) setError(reason instanceof Error ? reason.message : String(reason));
      }
    })();
    onCleanup(() => {
      disposed = true;
      window.Snowglass.controllers.removeEventListener("statechange", changed);
      session()?.removeEventListener("change", changed);
      if (!leaving && session()?.active) void session()!.commit();
    });
  });

  return <PauseDecorator releaseRef={(release) => { releasePause = release; }}>
    <Dialog.Root open modal={false} closeOnEscape={false} preventScroll={false}>
      <OverlayPortal><Dialog.Positioner class="horizon-order-positioner">
        <Dialog.Content ref={content} class="horizon-order-screen horizon-input-scope" onKeyDown={(event: KeyboardEvent) => {
          if (event.key === "Escape" || event.key === "Enter") {
            event.preventDefault();
            event.stopPropagation();
            void commit();
          }
        }}>
          <Dialog.Title class="horizon-order-visually-hidden">Change Grip / Order</Dialog.Title>
          <Dialog.Description class="horizon-order-visually-hidden">Connect controllers in player order.</Dialog.Description>

          <Show when={!configuring()} fallback={
            <section class="horizon-order-configuring" role="status" aria-live="polite">
              <i class="horizon-order-configuring-spinner" aria-hidden="true" />
              <h2>Configuring Controllers</h2>
              <p>Applying the new controller configuration…</p>
            </section>
          }>
          <header class="horizon-order-modes" aria-hidden="true">
            <div class="horizon-order-mode"><img src={controllerSwitch} alt="" /></div>
            <div class="horizon-order-mode selected">
              <span class="horizon-order-sideways"><img src={joyconLeft} alt="" /><img src={joyconRight} alt="" /></span>
              <strong>Two players also supported.</strong>
            </div>
            <div class="horizon-order-mode"><img src={controllerPro} alt="" /></div>
          </header>

          <main class="horizon-order-main">
            <p class="horizon-order-join-prompt">Press <img src={buttonL} alt="L" /> <span>+</span> <img src={buttonR} alt="R" /> on the controller.</p>
            <section class="horizon-order-players" aria-label="Player order">
              <For each={players()}>{(player) => <div class="horizon-order-slot-wrap" classList={{ assigned: Boolean(player.controller) }}>
                <div class="horizon-order-slot" aria-label={player.controller ? `Player ${player.index + 1}, ${player.controller.name}` : `Player ${player.index + 1}, not assigned`}>
                  <span class="horizon-order-player-number">{player.index + 1}</span>
                  <Show when={player.controller}>{(controller) => <>
                    <img class="horizon-order-assigned-controller" src={controllerPro} alt="" />
                    <strong>{controller().name}</strong>
                  </>}</Show>
                </div>
              </div>}</For>
            </section>

            <Show when={candidates().length > 0}>
              <section class="horizon-order-unassigned" aria-label="Discovered controllers">
                <span>Discovered:</span>
                <For each={candidates()}>{(candidate) => <button
                  type="button"
                  classList={{ pairing: isPairing(candidate) }}
                  aria-busy={isPairing(candidate)}
                  disabled={pending() || isPairing(candidate)}
                  onClick={() => void connect(candidate)}
                >
                  <img src={controllerPro} alt="" /><span>{candidate.name}</span>
                  <Show when={isPairing(candidate)}><i class="horizon-order-pairing-spinner" aria-hidden="true" /></Show>
                </button>}</For>
              </section>
            </Show>

            <Show when={!session()}><p class="horizon-order-loading">{error() || "Preparing controllers…"}</p></Show>
            <Show when={error() && session()}><p class="horizon-order-error" role="alert">{error()}</p></Show>
            <button type="button" class="horizon-order-ready" data-input-default disabled={!session() || pending()} onClick={() => void commit()}>
              Press <img src={buttonA} alt="A" /> when you're ready.
            </button>
          </main>

          <footer class="horizon-order-footer">
            <div class="horizon-order-radio-status" aria-label="Controller connection availability">
              <span classList={{ unavailable: availability().osBle === "unavailable" }}><img src={controllerSwitch} alt="" /> Bluetooth</span>
              <span classList={{ unavailable: availability().winUsb === "unavailable" }}><img src={controllerPro} alt="" /> USB receiver</span>
              <Show when={discoveryActive()}><strong><i /> Searching</strong></Show>
            </div>
            <div class="horizon-order-help"><span><img src={buttonMinus} alt="Minus" /><img src={buttonPlus} alt="Plus" /></span> Controller not connecting?</div>
            <button type="button" class="horizon-order-back" onClick={() => void commit()}><img src={buttonB} alt="B" /> Back</button>
          </footer>
          </Show>
        </Dialog.Content>
      </Dialog.Positioner></OverlayPortal>
    </Dialog.Root>
  </PauseDecorator>;
}
