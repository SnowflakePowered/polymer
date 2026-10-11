import type { ControllersApi, InputApi } from "./polymer";
import type { Direction, InputAction, NavigationRouter } from "./navigation";

const BUTTONS = [
  [0, "activate"], [1, "back"], [2, "details"], [5, "pause"], [9, "previous"], [10, "next"],
  [11, "up"], [12, "down"], [13, "left"], [14, "right"],
] as const;
const DIRECTIONS: Record<string, Direction> = { ArrowLeft: "left", ArrowRight: "right", ArrowUp: "up", ArrowDown: "down" };
const KEYS: Record<string, InputAction["type"]> = { Enter: "activate", " ": "activate", Escape: "back", Shift: "details", PageUp: "previous", PageDown: "next" };

/**
 * Translates keyboard and controller state into semantic actions for a router.
 * Directional input repeats after 350 ms and then every 90 ms until released or the scopes change.
 */
export class InputActionSource {
  private timers = new Map<string, ReturnType<typeof setTimeout>>();
  private axes = new Map<string, number>();
  private buttons = new Map<number, number>();
  private released = new Map<string, number>();

  constructor(
    private polymer: Readonly<{ input: InputApi; controllers: ControllersApi }>,
    private router: NavigationRouter,
  ) {}

  start() {
    this.polymer.controllers.addEventListener("statechange", this.clear);
    this.polymer.controllers.addEventListener("playerchange", this.clear);
    this.polymer.input.addEventListener("input", this.input);
    this.router.addEventListener("revision", this.clear);
    window.addEventListener("keydown", this.keyboard);
    window.addEventListener("blur", this.clear);
    document.addEventListener("focusin", this.router.focusChanged);
  }

  dispose() {
    this.clear();
    this.polymer.controllers.removeEventListener("statechange", this.clear);
    this.polymer.controllers.removeEventListener("playerchange", this.clear);
    this.polymer.input.removeEventListener("input", this.input);
    this.router.removeEventListener("revision", this.clear);
    window.removeEventListener("keydown", this.keyboard);
    window.removeEventListener("blur", this.clear);
    document.removeEventListener("focusin", this.router.focusChanged);
  }

  private readonly clear = () => {
    for (const timer of this.timers.values()) clearTimeout(timer);
    this.timers.clear();
  };

  private stop(key: string) {
    clearTimeout(this.timers.get(key));
    this.timers.delete(key);
  }

  private emit(key: string, action: InputAction) {
    const revision = this.router.revision;
    this.router.dispatch(action);
    if (action.type !== "move" || this.router.revision !== revision) return;
    this.stop(key);
    const repeat = () => {
      this.router.dispatch({ ...action, event: new Event("controller-repeat", { cancelable: true }) });
      if (this.timers.has(key)) this.timers.set(key, setTimeout(repeat, 90));
    };
    this.timers.set(key, setTimeout(repeat, 350));
  }

  private readonly input = (event: Event) => {
    const index = (event as CustomEvent<{ index: number }>).detail.index;
    const state = this.polymer.input.get(index);
    if (!state) { this.clear(); return; }
    const sourceId = this.polymer.controllers.players.find(player => player.index === index)?.controller?.id ?? null;
    const previous = this.buttons.get(index) ?? 0;
    this.buttons.set(index, state.buttons);
    for (const [bit, name] of BUTTONS) {
      const key = `${index}:button:${bit}`, pressed = !!(state.buttons & (1 << bit));
      if (!pressed) { this.stop(key); if (previous & (1 << bit)) this.released.set(key, performance.now()); continue; }
      if (previous & (1 << bit) || performance.now() - (this.released.get(key) ?? -Infinity) < 50) continue;
      const moving = bit >= 11;
      this.emit(key, { type: moving ? "move" : name as InputAction["type"], direction: moving ? name as Direction : undefined, origin: "controller", sourceId, event: new Event("controller-action", { cancelable: true }) });
    }
    [state.leftStick, state.rightStick].forEach((stick, stickIndex) => stick.forEach((value, axis) => {
      const key = `${index}:stick:${stickIndex}:${axis}`, prior = this.axes.get(key) ?? 0;
      const direction = Math.abs(value) >= 12000 ? Math.sign(value) : Math.abs(value) < 8000 ? 0 : prior;
      if (direction === prior) return;
      this.axes.set(key, direction);
      this.stop(key);
      if (!direction) return;
      const moved: Direction = axis === 0 ? direction < 0 ? "left" : "right" : direction < 0 ? "up" : "down";
      this.emit(key, { type: "move", direction: moved, origin: "controller", sourceId, event: new Event("controller-action", { cancelable: true }) });
    }));
  };

  private readonly keyboard = (event: KeyboardEvent) => {
    if (event.defaultPrevented || event.isComposing || event.altKey || event.ctrlKey || event.metaKey) return;
    const target = event.target;
    if (event.key !== "Escape" && target instanceof Element && target.closest("input:not([type='checkbox']), textarea, select, [contenteditable='true']")) return;
    if (event.repeat && !DIRECTIONS[event.key]) return;
    const type = DIRECTIONS[event.key] ? "move" : KEYS[event.key];
    if (type) this.router.dispatch({ type, direction: DIRECTIONS[event.key], origin: "keyboard", sourceId: null, event });
  };
}
