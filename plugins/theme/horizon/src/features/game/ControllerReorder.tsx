import { DialogParts as Dialog, Button, Spinner, ControllerIllustration, ButtonChord, ButtonGlyph } from "../../ui";
import { For, Show, createSignal, onCleanup, onMount } from "solid-js";
import type { InputSourceSelector, ManagedControllerDevice } from "@polymer-fe/frontend";
import { ControllerOrder } from "@polymer-fe/frontend/controller-order";
import buttonA from "../../assets/switch/button-a.svg";
import buttonB from "../../assets/switch/button-b.svg";
import buttonL from "../../assets/switch/button-l.svg";
import buttonMinus from "../../assets/switch/button-minus.svg";
import buttonPlus from "../../assets/switch/button-plus.svg";
import buttonR from "../../assets/switch/button-r.svg";
import controllerPro from "../../assets/switch/controller-pro.svg";
import controllerSwitch from "../../assets/switch/controller-switch.svg";
import { useShell } from "../../app/shell-context";
import { OverlayPortal, type InputAction } from "../../ui";
import { PauseDecorator } from "./PauseDecorator";
import type { PauseRelease } from "@polymer-fe/frontend/pause";
import "./ControllerReorder.css";

const NAVIGATION_KEYS = new Set(["Escape", "Enter", "Tab", " ", "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"]);

export function ControllerReorder(props: { onClose?: () => void }) {
  const shell = useShell();
  const order = new ControllerOrder(window.Polymer.controllers, (shell.model.request ?? shell.model.launch?.request)?.controllerTopology.mappings ?? []);
  const [revision, setRevision] = createSignal(0);
  const [pending, setPending] = createSignal(false);
  const [configuring, setConfiguring] = createSignal(false);
  const [error, setError] = createSignal("");
  let leaving = false;
  let releasePause: PauseRelease | undefined;
  let content: HTMLDivElement | undefined;

  const session = () => { revision(); return order.session; };
  const players = () => { revision(); return order.players; };
  const candidates = () => { revision(); return order.candidates; };
  const availability = () => { revision(); return order.availability; };
  const discoveryActive = () => { revision(); return order.discovering; };
  const lesserGroup = () => { revision(); return order.lesserGroup; };
  const groups = () => order.groups;
  const controllerAt = (index: number) => { revision(); return order.controllerAt(index); };
  const slots = () => { revision(); return order.slots; };
  const isPairing = (candidate: ManagedControllerDevice) => candidate.pairing === true;
  const fail = (reason: unknown) => setError(reason instanceof Error ? reason.message : String(reason));
  const join = (source: InputSourceSelector) => order.join(source).catch(fail);

  const closeOverlay = () => {
    if (props.onClose) props.onClose();
    else {
      shell.setModel("pauseOpen", false);
      shell.navigate(shell.gamePath("play"), true);
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
      await order.commit();
      await releasePause?.();
      closeOverlay();
    } catch (reason) {
      leaving = false;
      setPending(false);
      setConfiguring(false);
      fail(reason);
    }
  };

  const connect = async (candidate: ManagedControllerDevice) => {
    setPending(true);
    setError("");
    try {
      await order.connect(candidate.id);
    } catch (reason) {
      fail(reason);
    } finally {
      setPending(false);
    }
  };

  onMount(() => {
    const changed = () => setRevision((value) => value + 1);
    const failed = (event: Event) => fail((event as CustomEvent<unknown>).detail);
    order.addEventListener("change", changed);
    order.addEventListener("error", failed);
    void order.start().then(() => requestAnimationFrame(() => content?.querySelector<HTMLElement>(".horizon-order-main")?.focus()), fail);
    onCleanup(() => {
      order.removeEventListener("change", changed);
      order.removeEventListener("error", failed);
      order.dispose();
    });
  });

  return <PauseDecorator releaseRef={(release) => { releasePause = release; }}>
    <Dialog.Root open onOpenChange={details => { if (!details.open && !pending()) void commit(); }} closeOnEscape={!pending()} preventScroll={false}>
      <OverlayPortal><Dialog.Positioner class="horizon-order-positioner">
        <Dialog.Content ref={content} class="horizon-order-screen horizon-input-scope" onAction={(action: InputAction) => {
          if (action.type !== "activate" || pending() || !session()) return false;
          const enter = action.event instanceof KeyboardEvent && action.event.key === "Enter";
          if (!enter && document.activeElement?.closest("button") && content?.contains(document.activeElement)) return false;
          void commit();
          return true;
        }} onKeyDown={(event: KeyboardEvent) => {
          if (event.key === "Escape") {
            event.preventDefault();
            event.stopPropagation();
            if (!pending()) void commit();
          } else if (!NAVIGATION_KEYS.has(event.key) && !event.repeat && session() && !pending()
            && !players().some((player) => player.controller?.id === "keyboardMouse")) {
            void join({ kind: "keyboardMouse" });
          }
        }}>
          <Dialog.Title class="horizon-order-visually-hidden">Change Grip / Order</Dialog.Title>
          <Dialog.Description class="horizon-order-visually-hidden">Connect controllers in player order.</Dialog.Description>

          <Show when={!configuring()} fallback={
            <section class="horizon-order-configuring" role="status" aria-live="polite">
              <Spinner label="Configuring controllers" />
              <h2>Configuring Controllers</h2>
              <p>Applying the new controller configuration…</p>
            </section>
          }>
          <main class="horizon-order-main" tabindex="-1" data-input-default>
            <p class="horizon-order-join-prompt">Press <ButtonChord buttons={["l", "r"]} /> on the controller.</p>
            <div class="horizon-order-roster">
              <Show when={groups().length > 1}>
                <div class="horizon-order-lesser-groups">
                  <For each={groups().slice(1)}>{(group) => <Button
                    type="button"
                    class="horizon-order-lesser"
                    classList={{ selected: lesserGroup() === group[0].target.group }}
                    aria-pressed={lesserGroup() === group[0].target.group}
                    aria-label={`Connect ${group[0].target.group} controllers`}
                    onClick={() => order.setLesserGroup(lesserGroup() === group[0].target.group ? null : group[0].target.group)}
                  >
                    <For each={group}>{(mapping) => <span
                      data-connected={!!controllerAt(mapping.gamepad) || undefined}
                      title={controllerAt(mapping.gamepad)?.name ?? `${mapping.target.group} ${mapping.target.port + 1}`}
                    >{mapping.target.port + 1}</span>}</For>
                  </Button>}</For>
                </div>
              </Show>
              <section class="horizon-order-players" aria-label="Player order" data-inactive={lesserGroup() !== null || undefined} style={{ "--horizon-order-columns": Math.min(slots().length, 4) }}>
                <For each={slots()}>{(index, position) => <div class="horizon-order-slot" data-connected={!!controllerAt(index) || undefined}>
                  <Show when={controllerAt(index)} fallback={<span class="horizon-order-player-number">{position() + 1}</span>}>{(controller) => <>
                    <span class="horizon-order-player-lights" aria-label={`Player ${position() + 1}`}>
                      <For each={[0, 1, 2, 3]}>{(light) => <i data-lit={light <= position() || undefined} />}</For>
                    </span>
                    <ControllerIllustration kind="pro" label={controller().name} />
                    <strong>{controller().name}</strong>
                  </>}</Show>
                </div>}</For>
              </section>
            </div>

            <Show when={candidates().length > 0}>
              <section class="horizon-order-unassigned" aria-label="Discovered controllers">
                <span>Discovered:</span>
                <For each={candidates()}>{(candidate) => <Button
                  type="button"
                  classList={{ pairing: isPairing(candidate) }}
                  aria-busy={isPairing(candidate)}
                  disabled={pending() || isPairing(candidate)}
                  onClick={() => void connect(candidate)}
                >
                  <img src={controllerPro} alt="" /><span>{candidate.name}</span>
                  <Show when={isPairing(candidate)}><Spinner label="Pairing" /></Show>
                </Button>}</For>
              </section>
            </Show>

            <Show when={!session()}><p class="horizon-order-loading">{error() || "Preparing controllers…"}</p></Show>
            <Show when={error() && session()}><p class="horizon-order-error" role="alert">{error()}</p></Show>
            <p class="horizon-order-ready" data-disabled={!session() || pending() || undefined}>
              Press <ButtonGlyph button="a" /> when you're ready.
            </p>
          </main>

          <footer class="horizon-order-footer">
            <div class="horizon-order-radio-status" aria-label="Controller connection availability">
              <span classList={{ unavailable: availability().osBle === "unavailable" }}><img src={controllerSwitch} alt="" /> Bluetooth</span>
              <span classList={{ unavailable: availability().winUsb === "unavailable" }}><img src={controllerPro} alt="" /> USB receiver</span>
              <Show when={discoveryActive()}><strong><i /> Searching</strong></Show>
            </div>
            <div class="horizon-order-help"><span><img src={buttonMinus} alt="Minus" /><img src={buttonPlus} alt="Plus" /></span> Controller not connecting?</div>
            <Button type="button" class="horizon-order-back" onClick={() => void commit()}><ButtonGlyph button="b" /> Back</Button>
          </footer>
          </Show>
        </Dialog.Content>
      </Dialog.Positioner></OverlayPortal>
    </Dialog.Root>
  </PauseDecorator>;
}
