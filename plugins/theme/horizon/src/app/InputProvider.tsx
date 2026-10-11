import { onCleanup, onMount, type ParentProps } from "solid-js";
import { InputActionSource } from "@polymer-fe/frontend/input-actions";
import { NavigationController, NavigationProvider } from "../ui/input";
export type { InputAction } from "../ui/input";

export function InputProvider(props: ParentProps) {
  const navigation = new NavigationController();
  const source = new InputActionSource(window.Polymer, navigation);
  onMount(() => {
    source.start();
    onCleanup(() => source.dispose());
  });
  return <NavigationProvider controller={navigation}>{props.children}</NavigationProvider>;
}
