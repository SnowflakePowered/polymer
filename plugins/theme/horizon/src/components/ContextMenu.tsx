import { For, Show, onCleanup, onMount } from "solid-js";
import "./ContextMenu.css";

export type ContextMenuAction = { label: string; danger?: boolean; run(): void };

export function ContextMenu(props: { title: string; description?: string; actions: ContextMenuAction[]; onClose(): void }) {
  let menu!: HTMLDivElement;
  onMount(() => {
    const previous = document.activeElement;
    menu.querySelector<HTMLButtonElement>("button")?.focus();
    onCleanup(() => { if (previous instanceof HTMLElement && previous.isConnected) previous.focus(); });
  });
  return <div class="horizon-context-backdrop" onClick={props.onClose} onContextMenu={event => { event.preventDefault(); props.onClose(); }}>
    <div ref={menu} class="horizon-context-menu horizon-input-scope" role="menu" aria-label={props.title}
      onClick={event => event.stopPropagation()}
      onKeyDown={event => {
        if (event.key === "Escape" || event.key === "Tab") { event.preventDefault(); event.stopPropagation(); props.onClose(); }
      }}>
      <div class="horizon-context-title">{props.title}</div>
      <Show when={props.description}><div class="horizon-context-description">{props.description}</div></Show><div class="horizon-context-actions"><For each={props.actions}>{action => <button type="button" role="menuitem" classList={{ danger: action.danger }} onClick={() => { action.run(); props.onClose(); }}>{action.label}</button>}</For></div>
    </div>
  </div>;
}
