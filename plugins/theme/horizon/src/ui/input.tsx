import { createContext, createEffect, createSignal, onCleanup, onMount, useContext, type Accessor, type Context, type ParentProps } from "solid-js";
import { NavigationRouter, type ScopeOptions } from "../../../../../js/navigation";
export { embeddedRuntime } from "../../../../../js/navigation";
export type { ActionHint, ActionName, Direction, FocusRingDelegate, InputAction, ScopeOptions } from "../../../../../js/navigation";

/** Exposes the router's revision, modality and focus as Solid signals. */
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

const NavigationContext: Context<NavigationController | undefined> = import.meta.hot?.data.navigationContext ?? createContext<NavigationController>();
if (import.meta.hot) import.meta.hot.data.navigationContext = NavigationContext;
export function NavigationProvider(props: ParentProps<{ controller?: NavigationController }>) {
  const controller = props.controller ?? new NavigationController();
  const focusout = () => queueMicrotask(controller.updateRing);
  onMount(() => {
    document.addEventListener("keydown", controller.trapTab); document.addEventListener("focusout", focusout);
    onCleanup(() => { document.removeEventListener("keydown", controller.trapTab); document.removeEventListener("focusout", focusout); });
  });
  return <NavigationContext.Provider value={controller}>{props.children}</NavigationContext.Provider>;
}
export function useNavigation() {
  const value = useContext(NavigationContext);
  if (!value) throw new Error("Horizon UI requires NavigationProvider");
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
