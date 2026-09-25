import "../index.css";
import { render } from "solid-js/web";
import { EnvironmentProvider } from "@ark-ui/solid";
import { onCleanup, onMount } from "solid-js";
import { NavigationProvider, useNavigation, type Direction, type ActionName } from "../ui";
import { Gallery } from "./Gallery";

function GalleryInput() {
  const navigation = useNavigation();
  onMount(() => {
    const keyboard = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.isComposing || event.ctrlKey || event.altKey || event.metaKey) return;
      if (event.key !== "Escape" && (event.target as HTMLElement)?.closest("input, textarea, select, [contenteditable='true']")) return;
      const directions: Record<string, Direction> = { ArrowUp: "up", ArrowDown: "down", ArrowLeft: "left", ArrowRight: "right" };
      const actions: Record<string, ActionName> = { Enter: "activate", " ": "activate", Escape: "back", PageUp: "previous", PageDown: "next" };
      const type = directions[event.key] ? "move" : actions[event.key];
      if (type) navigation.dispatch({ type, direction: directions[event.key], origin: "keyboard", sourceId: null, event });
    };
    window.addEventListener("keydown", keyboard); document.addEventListener("focusin", navigation.focusChanged);
    onCleanup(() => { window.removeEventListener("keydown", keyboard); document.removeEventListener("focusin", navigation.focusChanged); });
  });
  return <Gallery />;
}
if (import.meta.env.DEV) render(() => <EnvironmentProvider value={() => document}><NavigationProvider><GalleryInput /></NavigationProvider></EnvironmentProvider>, document.getElementById("root")!);
