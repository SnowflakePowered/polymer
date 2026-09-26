import { createEffect, onCleanup, onMount, type ParentProps } from "solid-js";
import { NavigationController, NavigationProvider, type InputAction, type Direction } from "../ui/input";
export type { InputAction } from "../ui/input";
export type InputDirection = Direction;
export type InputOrigin = InputAction["origin"];
type ActionListener = (action: InputAction) => void;
/** Host state stays here; the UI receives only semantic actions. */
class ControllerRepeat {
  private timers = new Map<string, ReturnType<typeof setTimeout>>();
  constructor(private emit: ActionListener) {}
  start(key: string, action: InputAction) {
    this.stop(key);
    const repeat = () => {
      this.emit({ ...action, event: new Event("controller-repeat", { cancelable: true }) });
      if (this.timers.has(key)) this.timers.set(key, setTimeout(repeat, 90));
    };
    this.timers.set(key, setTimeout(repeat, 350));
  }
  stop(key: string) { clearTimeout(this.timers.get(key)); this.timers.delete(key); }
  clear() { for (const timer of this.timers.values()) clearTimeout(timer); this.timers.clear(); }
}
export function InputProvider(props: ParentProps) {
  const navigation = new NavigationController();
  const emit = (action: InputAction) => { navigation.dispatch(action); };
  const repeat = new ControllerRepeat(emit);
  createEffect(() => { navigation.version(); repeat.clear(); });
  onMount(() => {
    const axes = new Map<string, number>(), buttons = new Map<number, number>(), released = new Map<string, number>();
    const reset = () => { repeat.clear(); };
    const input = (event: Event) => {
      const index = (event as CustomEvent<{ index: number }>).detail.index;
      const state = window.Polymer.input.get(index);
      if (!state) { reset(); return; }
      const sourceId = window.Polymer.controllers.players.find(player => player.index === index)?.controller?.id ?? null;
      const previous = buttons.get(index) ?? 0;
      buttons.set(index, state.buttons);
      const mapping = [
        [0, "activate"], [1, "back"], [2, "details"], [5, "pause"], [9, "previous"], [10, "next"],
        [11, "up"], [12, "down"], [13, "left"], [14, "right"],
      ] as const;
      for (const [bit, name] of mapping) {
        const key = `${index}:button:${bit}`, pressed = !!(state.buttons & (1 << bit));
        if (!pressed) { repeat.stop(key); if (previous & (1 << bit)) released.set(key, performance.now()); continue; }
        if (previous & (1 << bit)) continue;
        if (performance.now() - (released.get(key) ?? -Infinity) < 50) continue;
        const moving = bit >= 11;
        const action: InputAction = { type: moving ? "move" : name as InputAction["type"], direction: moving ? name as Direction : undefined, origin: "controller", sourceId, event: new Event("controller-action", { cancelable: true }) };
        const scopeVersion = navigation.version();
        emit(action);
        if (moving && navigation.version() === scopeVersion) repeat.start(key, action);
      }
      [state.leftStick, state.rightStick].forEach((stick, stickIndex) => stick.forEach((value, axis) => {
        const key = `${index}:stick:${stickIndex}:${axis}`, prior = axes.get(key) ?? 0;
        const direction = Math.abs(value) >= 12000 ? Math.sign(value) : Math.abs(value) < 8000 ? 0 : prior;
        if (direction === prior) return;
        axes.set(key, direction); repeat.stop(key);
        if (!direction) return;
        const action: InputAction = { type: "move", direction: axis === 0 ? direction < 0 ? "left" : "right" : direction < 0 ? "up" : "down", origin: "controller", sourceId, event: new Event("controller-action", { cancelable: true }) };
        const scopeVersion = navigation.version();
        emit(action); if (navigation.version() === scopeVersion) repeat.start(key, action);
      }));
    };
    const keyboard = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.isComposing || event.altKey || event.ctrlKey || event.metaKey) return;
      const target = event.target as HTMLElement | null;
      if (event.key !== "Escape" && target?.closest("input:not([type='checkbox']), textarea, select, [contenteditable='true']")) return;
      const directions: Record<string, Direction> = { ArrowLeft: "left", ArrowRight: "right", ArrowUp: "up", ArrowDown: "down" };
      const actions: Record<string, InputAction["type"]> = { Enter: "activate", " ": "activate", Escape: "back", Shift: "details", PageUp: "previous", PageDown: "next" };
      if (event.repeat && !directions[event.key]) return;
      const type = directions[event.key] ? "move" : actions[event.key];
      if (type) emit({ type, direction: directions[event.key], origin: "keyboard", sourceId: null, event });
    };
    const controllerStateChanged = () => repeat.clear();
    window.Polymer.controllers.addEventListener("statechange", controllerStateChanged);
    window.Polymer.input.addEventListener("input", input);
    window.Polymer.controllers.addEventListener("playerchange", reset);
    window.addEventListener("keydown", keyboard);
    window.addEventListener("blur", reset);
    document.addEventListener("focusin", navigation.focusChanged);
    onCleanup(() => {
      window.Polymer.controllers.removeEventListener("statechange", controllerStateChanged);
      reset(); window.Polymer.input.removeEventListener("input", input);
      window.Polymer.controllers.removeEventListener("playerchange", reset);
      window.removeEventListener("keydown", keyboard); window.removeEventListener("blur", reset);
      document.removeEventListener("focusin", navigation.focusChanged);
    });
  });
  return <NavigationProvider controller={navigation}>{props.children}</NavigationProvider>;
}
