import { Dialog as ArkDialog, Popover as ArkPopover, FloatingPanel as ArkFloating, Menu, useDialogContext, usePopoverContext, useFloatingPanelContext, useMenu } from "@ark-ui/solid";
import { For, Show, createEffect, createSignal, splitProps, type JSX, type ParentProps } from "solid-js";
import { Button, ActionRow } from "./actions";
import { embeddedRuntime, useInputScope, type ActionHint, type InputAction } from "./input";
import { OverlayPortal } from "./portal";

function DialogContent(props: ArkDialog.ContentProps & { onAction?: (action: InputAction) => boolean; returnFocus?: () => HTMLElement | null | undefined; hints?: () => readonly ActionHint[] }) {
  const [local, rest] = splitProps(props, ["ref", "class", "onAction", "returnFocus", "hints"]);
  const api = useDialogContext();
  let root!: HTMLDivElement;
  useInputScope(() => ({ root, enabled: api().open, overlay: true, navigation: true, returnFocus: local.returnFocus,
    initialFocus: () => root.contains(document.activeElement) ? document.activeElement as HTMLElement : root.querySelector<HTMLElement>("[data-input-default], [autofocus]"),
    hints: () => local.hints?.() ?? [{ action: "activate", label: "Select" }, { action: "back", label: "Back" }],
    handle: action => { if (local.onAction?.(action)) return true; if (action.type !== "back") return false; api().setOpen(false); return true; } }));
  return <ArkDialog.Content {...rest} class={`hz-dialog ${local.class ?? ""}`} ref={element => { root = element; if (typeof local.ref === "function") local.ref(element); }} />;
}
function DialogRoot(props: ArkDialog.RootProps) {
  // Embedded MutationObserver records cannot currently support Ark's focus trap.
  // Input scopes enforce the same boundary in that environment.
  return <ArkDialog.Root trapFocus={!embeddedRuntime} {...props} />;
}
export const DialogParts = { ...ArkDialog, Root: DialogRoot, Content: DialogContent };
export type DialogProps = ParentProps<{ open: boolean; onOpenChange(open: boolean): void; title: JSX.Element; description?: string; actions?: JSX.Element; class?: string; initialFocus?: () => HTMLElement | null; returnFocus?: () => HTMLElement | null | undefined; pending?: boolean; onAction?: (action: InputAction) => boolean }>;
export function Dialog(props: DialogProps) {
  return <DialogRoot open={props.open} onOpenChange={details => { if (details.open || !props.pending) props.onOpenChange(details.open); }} initialFocusEl={props.initialFocus} closeOnInteractOutside={!props.pending} closeOnEscape={!props.pending}>
    <OverlayPortal><ArkDialog.Backdrop class="hz-scrim" /><ArkDialog.Positioner class="hz-dialog-positioner"><DialogContent class={props.class} onAction={props.onAction} returnFocus={props.returnFocus}>
      <ArkDialog.Title class="hz-dialog-title">{props.title}</ArkDialog.Title><Show when={props.description}><ArkDialog.Description class="hz-description">{props.description}</ArkDialog.Description></Show>
      <div class="hz-dialog-body">{props.children}</div><Show when={props.actions}><div class="hz-actions">{props.actions}</div></Show>
    </DialogContent></ArkDialog.Positioner></OverlayPortal>
  </DialogRoot>;
}
export function ConfirmDialog(props: { open: boolean; title: string; description: string; confirmLabel: string; pending?: boolean; returnFocus?: () => HTMLElement | null | undefined; onConfirm(): void; onClose(): void }) {
  let cancel!: HTMLButtonElement;
  return <Dialog open={props.open} onOpenChange={open => { if (!open && !props.pending) props.onClose(); }} title={props.title} description={props.description} pending={props.pending} returnFocus={props.returnFocus} initialFocus={() => cancel}
    actions={<><Button ref={cancel} disabled={props.pending} onClick={props.onClose}>Cancel</Button><Button appearance="danger" pending={props.pending} onClick={props.onConfirm}>{props.confirmLabel}</Button></>} />;
}
export type MenuAction = { id?: string; label: string; description?: string; danger?: boolean; disabled?: boolean; run(): void };
export function ActionDialog(props: { open: boolean; title: string; description?: string; actions: readonly MenuAction[]; returnFocus?: () => HTMLElement | null | undefined; onClose(): void }) {
  return <Dialog open={props.open} title={props.title} description={props.description} returnFocus={props.returnFocus} onOpenChange={open => { if (!open) props.onClose(); }}><For each={props.actions}>{action => <ActionRow appearance={action.danger ? "danger" : "secondary"} disabled={action.disabled} description={action.description} onClick={() => { action.run(); props.onClose(); }}>{action.label}</ActionRow>}</For></Dialog>;
}
export function SidePanel(props: DialogProps) { return <Dialog {...props} class={`hz-side-panel ${props.class ?? ""}`} />; }
function PopoverContent(props: ArkPopover.ContentProps) {
  const [local, rest] = splitProps(props, ["ref", "class"]);
  let root!: HTMLDivElement;
  const api = usePopoverContext();
  useInputScope(() => ({ root, enabled: api().open, overlay: true, navigation: true, handle: action => { if (action.type !== "back") return false; api().setOpen(false); return true; } }));
  return <ArkPopover.Content {...rest} class={`hz-popover ${local.class ?? ""}`} ref={element => { root = element; if (typeof local.ref === "function") local.ref(element); }} />;
}
export const Popover = { ...ArkPopover, Content: PopoverContent };
function FloatingContent(props: ArkFloating.ContentProps) {
  const [local, rest] = splitProps(props, ["class", "ref"]);
  let root!: HTMLDivElement;
  const api = useFloatingPanelContext();
  useInputScope(() => ({ root, enabled: api().open, navigation: true, handle: action => { if (action.type !== "back") return false; api().setOpen(false); return true; } }));
  return <ArkFloating.Content {...rest} class={`hz-floating-panel ${local.class ?? ""}`} ref={element => { root = element; if (typeof local.ref === "function") local.ref(element); }} />;
}
export const FloatingPanel = { ...ArkFloating, Content: FloatingContent };
export function ContextMenu(props: ParentProps<{ label: string; actions: readonly MenuAction[] }>) {
  let root!: HTMLDivElement, content!: HTMLDivElement;
  const [open, setOpen] = createSignal(false);
  const api = useMenu(() => ({ open: open(), onOpenChange: details => setOpen(details.open), onSelect: details => props.actions.find((item, index) => (item.id ?? String(index)) === details.value)?.run() }));
  createEffect(() => { if (open()) { const index = props.actions.findIndex(item => !item.disabled); if (index >= 0) queueMicrotask(() => api.api().setHighlightedValue(props.actions[index].id ?? String(index))); } });
  useInputScope(() => ({ root: content, enabled: open(), overlay: true, navigation: true, handle: action => {
    if (action.type === "back") { setOpen(false); return true; }
    if (action.origin !== "controller") return false;
    const items = props.actions.map((item, index) => ({ ...item, value: item.id ?? String(index) })).filter(item => !item.disabled);
    if (action.type === "activate") { const item = items.find(item => item.value === api.api().highlightedValue); item?.run(); setOpen(false); return true; }
    if (action.type !== "move" || !["up", "down"].includes(action.direction ?? "")) return false;
    const index = items.findIndex(item => item.value === api.api().highlightedValue);
    const next = items[Math.max(0, Math.min(items.length - 1, index + (action.direction === "up" ? -1 : 1)))];
    if (next) api.api().setHighlightedValue(next.value);
    return true;
  } }));
  return <Menu.RootProvider value={api}><div ref={root}><Menu.ContextTrigger>{props.children}</Menu.ContextTrigger><Menu.Trigger class="hz-button">{props.label}</Menu.Trigger></div><OverlayPortal><Menu.Positioner class="hz-picker-positioner"><Menu.Content ref={content} class="hz-popup"><For each={props.actions}>{(item, index) => <Menu.Item value={item.id ?? String(index())} disabled={item.disabled} class="hz-option" data-danger={item.danger || undefined}>{item.label}</Menu.Item>}</For></Menu.Content></Menu.Positioner></OverlayPortal></Menu.RootProvider>;
}
