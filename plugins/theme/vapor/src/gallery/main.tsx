import "../ui/vapor.css";
import "./gallery.css";
import { render } from "solid-js/web";
import { EnvironmentProvider } from "@ark-ui/solid";
import { onCleanup, onMount } from "solid-js";
import { NavigationController, NavigationProvider, type ActionName, type Direction } from "../ui";
import { Gallery } from "./Gallery";

const directions: Record<string, Direction> = { ArrowUp: "up", ArrowDown: "down", ArrowLeft: "left", ArrowRight: "right" };
const actions: Record<string, ActionName> = { Enter: "activate", " ": "activate", Escape: "back", Shift: "details", PageUp: "previous", PageDown: "next", F1: "pause" };

function GalleryInput(props: { navigation: NavigationController }) {
  let controller = false;
  onMount(() => {
    const keyboard = (event: KeyboardEvent) => {
      if (event.key === "F2") { controller = !controller; event.preventDefault(); return; }
      if (event.defaultPrevented || event.isComposing || event.ctrlKey || event.altKey || event.metaKey) return;
      if (event.key !== "Escape" && (event.target as HTMLElement)?.closest("input:not([type='checkbox']), textarea, select")) return;
      const type = directions[event.key] ? "move" : actions[event.key];
      if (type) props.navigation.dispatch({ type, direction: directions[event.key], origin: controller ? "controller" : "keyboard", sourceId: null, event });
    };
    window.addEventListener("keydown", keyboard);
    document.addEventListener("focusin", props.navigation.focusChanged);
    onCleanup(() => { window.removeEventListener("keydown", keyboard); document.removeEventListener("focusin", props.navigation.focusChanged); });
  });
  return <Gallery />;
}

if (import.meta.env.DEV) {
  const navigation = new NavigationController();
  render(() => <EnvironmentProvider value={() => document}><NavigationProvider controller={navigation}><GalleryInput navigation={navigation} /></NavigationProvider></EnvironmentProvider>, document.getElementById("root")!);
}
