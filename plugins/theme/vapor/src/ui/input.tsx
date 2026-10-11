import { createContext, createEffect, createSignal, onCleanup, onMount, useContext, type Accessor, type ParentProps } from "solid-js";
import { NavigationRouter, type ScopeOptions } from "@polymer-fe/frontend/navigation";
export { embeddedRuntime } from "@polymer-fe/frontend/navigation";
export type { ActionHint, ActionName, Direction, InputAction, ScopeOptions } from "@polymer-fe/frontend/navigation";

export class NavigationController extends NavigationRouter {
  private readonly signals = {
    revision: createSignal(0),
    origin: createSignal<NavigationRouter["modality"]>("keyboard"),
    active: createSignal<Element | null>(null),
  };
  readonly version = this.signals.revision[0];
  readonly origin = this.signals.origin[0];
  readonly hints = () => { this.version(); this.signals.active[0](); return this.activeHints(); };
  readonly hasOverlay = () => { this.version(); return this.overlayOpen(); };

  constructor() {
    super();
    this.addEventListener("revision", () => this.signals.revision[1](this.revision));
    this.addEventListener("origin", () => this.signals.origin[1](this.modality));
    this.addEventListener("active", () => this.signals.active[1](this.active));
  }
}

const NavigationContext = createContext<NavigationController>();
export function NavigationProvider(props: ParentProps<{ controller: NavigationController }>) {
  const focusout = () => queueMicrotask(props.controller.updateRing);
  onMount(() => {
    document.addEventListener("keydown", props.controller.trapTab);
    document.addEventListener("focusout", focusout);
    onCleanup(() => { document.removeEventListener("keydown", props.controller.trapTab); document.removeEventListener("focusout", focusout); });
  });
  return <NavigationContext.Provider value={props.controller}>{props.children}</NavigationContext.Provider>;
}
export function useNavigation() {
  const value = useContext(NavigationContext);
  if (!value) throw new Error("Vapor UI requires NavigationProvider");
  return value;
}
export function useInputScope(options: Accessor<ScopeOptions>) {
  const navigation = useNavigation();
  createEffect(() => {
    const scope = options();
    if (scope.root && scope.enabled !== false) onCleanup(navigation.register(scope));
  });
}
export function useFocusRing(root: () => Element | undefined, target: () => Element | null | undefined) {
  const navigation = useNavigation();
  onCleanup(navigation.delegateRing({ root, target }));
  createEffect(() => { target(); queueMicrotask(navigation.updateRing); });
}
