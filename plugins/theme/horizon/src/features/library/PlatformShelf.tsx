import { For, createResource, onMount } from "solid-js";
import { useShell } from "../../app/shell-context";
import { Icon, useInputScope } from "../../ui";
import "./PlatformShelf.css";

function Chevron(props: { direction: "left" | "right"; onClick(): void }) {
  return <button type="button" class="horizon-shelf-arrow" tabIndex={-1} aria-hidden="true" data-direction={props.direction} onClick={props.onClick}>
    <svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
      <path d={props.direction === "left" ? "M15 5l-7 7 7 7" : "M9 5l7 7-7 7"} />
    </svg>
  </button>;
}

export function PlatformShelf(props: { onExit(): void }) {
  const shell = useShell();
  let root!: HTMLElement, track!: HTMLDivElement;
  const [platforms] = createResource(() => window.Polymer.library.games.platforms(), { initialValue: [] });
  const current = () => shell.model.libraryQuery.platforms[0] ?? null;
  const choose = (platform: string | null) => {
    shell.queryLibrary({ platforms: platform ? [platform] : [] });
    props.onExit();
  };
  const scroll = (direction: number) => track.scrollBy?.({ left: direction * track.clientWidth * .75, behavior: "smooth" });
  useInputScope(() => ({ root, navigation: true,
    hints: () => [{ action: "move", label: "Move" }, { action: "activate", label: "Select" }, { action: "back", label: "Close" }],
    handle: action => {
      if (action.type === "back" || (action.type === "move" && action.direction === "down")) { props.onExit(); return true; }
      if (action.type !== "move") return false;
      if (action.direction === "left" || action.direction === "right") {
        const cards = [...track.querySelectorAll<HTMLElement>(".horizon-shelf-card")];
        const card = cards[cards.indexOf(document.activeElement as HTMLElement) + (action.direction === "left" ? -1 : 1)];
        card?.focus();
        card?.scrollIntoView?.({ inline: "nearest", block: "nearest" });
      }
      return true;
    },
  }));
  onMount(() => requestAnimationFrame(() => {
    const card = track.querySelector<HTMLElement>("[aria-pressed='true']") ?? track.querySelector<HTMLElement>("button");
    card?.focus();
    card?.scrollIntoView?.({ inline: "center", block: "nearest" });
  }));
  return <section ref={root} class="horizon-shelf" onFocusOut={() => queueMicrotask(() => { if (root.isConnected && !root.contains(document.activeElement)) props.onExit(); })}>
    <Chevron direction="left" onClick={() => scroll(-1)} />
    <div ref={track} class="horizon-shelf-track" role="group" aria-label="Systems">
      <button type="button" class="horizon-shelf-card" aria-pressed={current() === null} onClick={() => choose(null)}>
        <span class="horizon-shelf-square horizon-shelf-all" aria-hidden="true"><Icon name="gamepad" tone="muted" strokeWidth={1.4} /><Icon name="gamepad" tone="muted" strokeWidth={1.4} /><Icon name="gamepad" tone="muted" strokeWidth={1.4} /></span>
        <span class="horizon-shelf-name">All systems</span>
      </button>
      <For each={platforms()}>{platform =>
        <button type="button" class="horizon-shelf-card" aria-pressed={current() === platform} onClick={() => choose(platform)}>
          <span class="horizon-shelf-square" aria-hidden="true"><Icon name="gamepad" tone="muted" strokeWidth={1.2} /></span>
          <span class="horizon-shelf-name">{window.Polymer.machine(platform)?.name ?? platform}</span>
        </button>
      }</For>
    </div>
    <Chevron direction="right" onClick={() => scroll(1)} />
  </section>;
}
