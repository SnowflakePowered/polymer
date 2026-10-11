import { Dialog as ArkDialog, Menu, useDialogContext, useMenu } from "@ark-ui/solid";
import { For, Show, createComputed, createEffect, createSignal, onCleanup, type JSX, type ParentProps } from "solid-js";
import { Portal } from "solid-js/web";
import { Button } from "./actions";
import { embeddedRuntime, useInputScope, type InputAction } from "./input";

type Dismisser = { content: () => HTMLElement | undefined; close(): void };
const dismissers: Dismisser[] = [];
const dismissOnClick = (event: MouseEvent) => {
  const top = dismissers.at(-1), content = top?.content();
  if (!top || !content || !(event.target instanceof Node) || content.contains(event.target)) return;
  event.preventDefault();
  event.stopPropagation();
  top.close();
};

/** Closes the topmost overlay on a click outside it. Pointer presses are ignored so the click cannot reach what was underneath. */
export function useClickOutside(content: () => HTMLElement | undefined, open: () => boolean, close: () => void) {
  createEffect(() => {
    if (!open()) return;
    const dismisser = { content, close };
    if (!dismissers.length) document.addEventListener("click", dismissOnClick, true);
    dismissers.push(dismisser);
    onCleanup(() => {
      dismissers.splice(dismissers.indexOf(dismisser), 1);
      if (!dismissers.length) document.removeEventListener("click", dismissOnClick, true);
    });
  });
}

export function OverlayPortal(props: ParentProps) {
  return <Portal ref={container => { container.style.display = "contents"; }}>{props.children}</Portal>;
}

function ScopedContent(props: ParentProps<{ class: string; onAction?: (action: InputAction) => boolean; initialFocus?: () => HTMLElement | null | undefined; returnFocus: () => HTMLElement | null }>) {
  const api = useDialogContext();
  let root!: HTMLDivElement;
  useClickOutside(() => root, () => api().open, () => {
    api().setOpen(false);
    requestAnimationFrame(() => { const opener = props.returnFocus(); if (opener?.isConnected) opener.focus(); });
  });
  useInputScope(() => ({ root, enabled: api().open, overlay: true, navigation: true, returnFocus: props.returnFocus,
    initialFocus: () => props.initialFocus?.() ?? root.querySelector<HTMLElement>("[data-autofocus], [autofocus]"),
    hints: () => [{ action: "activate", label: "Select" }, { action: "back", label: "Back" }],
    handle: action => { if (props.onAction?.(action)) return true; if (action.type !== "back") return false; api().setOpen(false); return true; } }));
  return <ArkDialog.Content ref={root} class={props.class}>{props.children}</ArkDialog.Content>;
}

type OverlayProps = ParentProps<{ open: boolean; onClose(): void; class?: string; onAction?: (action: InputAction) => boolean; initialFocus?: () => HTMLElement | null | undefined }>;

function Overlay(props: OverlayProps & { kind: "modal" | "left" | "right" }) {
  let opener: HTMLElement | null = null;
  createComputed(() => { if (props.open) opener = document.activeElement instanceof HTMLElement && document.activeElement !== document.body ? document.activeElement : null; });
  return <ArkDialog.Root open={props.open} trapFocus={!embeddedRuntime} closeOnInteractOutside={false} finalFocusEl={() => opener} onOpenChange={details => { if (!details.open) props.onClose(); }}>
    <OverlayPortal><ArkDialog.Backdrop class="vp-backdrop" data-kind={props.kind} /><ArkDialog.Positioner class="vp-overlay-position" data-kind={props.kind}>
      <ScopedContent class={`vp-overlay vp-${props.kind} ${props.class ?? ""}`} onAction={props.onAction} initialFocus={props.initialFocus} returnFocus={() => opener}>{props.children}</ScopedContent>
    </ArkDialog.Positioner></OverlayPortal>
  </ArkDialog.Root>;
}

/** GamepadDialog: centered card with a title, body and a row of dialog buttons. */
export function Modal(props: OverlayProps & { title: JSX.Element; description?: JSX.Element; buttons?: JSX.Element }) {
  return <Overlay {...props} kind="modal">
    <ArkDialog.Title class="vp-modal-title">{props.title}</ArkDialog.Title>
    <Show when={props.description}><ArkDialog.Description class="vp-modal-body">{props.description}</ArkDialog.Description></Show>
    {props.children}
    <Show when={props.buttons}><div class="vp-modal-buttons">{props.buttons}</div></Show>
  </Overlay>;
}

export function ConfirmModal(props: { open: boolean; title: string; description: string; confirm: string; danger?: boolean; onConfirm(): void; onClose(): void }) {
  return <Modal open={props.open} onClose={props.onClose} title={props.title} description={props.description}
    buttons={<><Button primary={!props.danger} danger={props.danger} data-autofocus onClick={() => { props.onConfirm(); props.onClose(); }}>{props.confirm}</Button><Button onClick={props.onClose}>Cancel</Button></>} />;
}

/** Slide-out panel anchored to the left (main menu) or right (quick access) edge. */
export function SidePanel(props: OverlayProps & { side: "left" | "right" }) {
  return <Overlay {...props} kind={props.side} />;
}

export type MenuAction = { label: string; danger?: boolean; disabled?: boolean; run(): void };

/** Steam's context menu: a centered list of actions over the dimmed screen. */
export function ContextMenu(props: { open: boolean; onClose(): void; title?: string; actions: readonly MenuAction[] }) {
  let content!: HTMLDivElement;
  const values = () => props.actions.map((action, index) => ({ ...action, value: String(index) })).filter(action => !action.disabled);
  const api = useMenu(() => ({ open: props.open, onOpenChange: details => { if (!details.open) props.onClose(); }, onInteractOutside: event => event.preventDefault(),
    onSelect: details => { props.actions[Number(details.value)]?.run(); props.onClose(); } }));
  const [ready, setReady] = createSignal(false);
  useClickOutside(() => content, () => props.open, () => props.onClose());
  createEffect(() => { if (props.open) queueMicrotask(() => { api.api().setHighlightedValue(values()[0]?.value ?? ""); setReady(true); }); else setReady(false); });
  useInputScope(() => ({ root: content, enabled: props.open && ready(), overlay: true, navigation: true, handle: action => {
    if (action.type === "back") { props.onClose(); return true; }
    if (action.origin !== "controller") return false;
    const items = values(), index = items.findIndex(item => item.value === api.api().highlightedValue);
    if (action.type === "activate") { items[index]?.run(); props.onClose(); return true; }
    if (action.type !== "move" || (action.direction !== "up" && action.direction !== "down")) return false;
    const next = items[Math.max(0, Math.min(items.length - 1, index + (action.direction === "up" ? -1 : 1)))];
    if (next) api.api().setHighlightedValue(next.value);
    return true;
  } }));
  return <Menu.RootProvider value={api}><OverlayPortal><div class="vp-menu-layer" data-open={props.open || undefined}><Menu.Content ref={content} class="vp-menu vp-context-menu">
    <Show when={props.title}><div class="vp-menu-title">{props.title}</div></Show>
    <For each={props.actions}>{(action, index) => <Menu.Item value={String(index())} disabled={action.disabled} class="vp-menu-item" data-danger={action.danger || undefined}><span>{action.label}</span></Menu.Item>}</For>
  </Menu.Content></div></OverlayPortal></Menu.RootProvider>;
}
