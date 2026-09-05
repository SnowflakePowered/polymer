import { createContext, onCleanup, onMount, useContext, type Context, type ParentProps } from "solid-js";

export type InputDirection = "left" | "right" | "up" | "down";
export type InputOrigin = "controller" | "keyboard";
export type InputAction =
  | { type: "move"; direction: InputDirection; origin: InputOrigin; sourceId: string | null; event: Event }
  | { type: "activate" | "back" | "details" | "pause"; origin: InputOrigin; sourceId: string | null; event: Event };

type ActionListener = (action: InputAction) => void;
type InputContextValue = { subscribe(listener: ActionListener): () => void };
type ControllerAction =
  | { type: "move"; direction: InputDirection }
  | { type: "activate" | "back" | "details" | "pause" };

const InputContext: Context<InputContextValue | undefined> = import.meta.hot?.data.inputContext ?? createContext<InputContextValue>();
if (import.meta.hot) import.meta.hot.data.inputContext = InputContext;
const controllerNavigationEvents = new WeakSet<Event>();
const buttonActions: Partial<Record<string, ControllerAction>> = {
  "directional.w": { type: "move", direction: "left" },
  "directional.e": { type: "move", direction: "right" },
  "directional.n": { type: "move", direction: "up" },
  "directional.s": { type: "move", direction: "down" },
  "button.a": { type: "activate" },
  "button.b": { type: "back" },
  "button.x": { type: "details" },
  "button.guide": { type: "pause" },
};

export function dispatchControllerBack(target: EventTarget) {
  const event = new KeyboardEvent("keydown", {
    key: "Escape",
    code: "Escape",
    bubbles: true,
    cancelable: true,
  });
  controllerNavigationEvents.add(event);
  target.dispatchEvent(event);
}
const axisDirections: Partial<Record<string, readonly [InputDirection, InputDirection]>> = {
  "analog.stick.left.x+": ["left", "right"],
  "analog.stick.left.y+": ["up", "down"],
  "analog.stick.right.x+": ["left", "right"],
  "analog.stick.right.y+": ["up", "down"],
};

export function useInputActions() {
  const context = useContext(InputContext);
  if (!context) throw new Error("Horizon input context is unavailable");
  return context;
}

export function InputProvider(props: ParentProps) {
  const listeners = new Set<ActionListener>();
  const emit = (action: InputAction) => listeners.forEach((listener) => listener(action));
  const subscribe = (listener: ActionListener) => {
    listeners.add(listener);
    return () => listeners.delete(listener);
  };

  onMount(() => {
    const axisLatch = new Map<string, -1 | 1>();
    const previousButtons = new Map<number, number>();
    const controllerInput = (event: Event) => {
      const { index } = (event as CustomEvent<{ index: number }>).detail;
      const state = window.Polymer.input.get(index);
      if (!state) return;
      const sourceId = window.Polymer.controllers.players.find((player) => player.index === index)?.controller?.id || null;
      const prior = previousButtons.get(index) || 0;
      previousButtons.set(index, state.buttons);
      const buttons = [
        [0, "button.a"], [1, "button.b"], [2, "button.x"], [5, "button.guide"],
        [11, "directional.n"], [12, "directional.s"], [13, "directional.w"], [14, "directional.e"],
      ] as const;
      for (const [bit, expression] of buttons) {
        if ((state.buttons & (1 << bit)) === 0 || (prior & (1 << bit)) !== 0) continue;
        const action = buttonActions[expression];
        if (action) emit({ ...action, origin: "controller", sourceId, event } as InputAction);
      }
      const axes = [
        ["analog.stick.left.x+", state.leftStick[0]], ["analog.stick.left.y+", state.leftStick[1]],
        ["analog.stick.right.x+", state.rightStick[0]], ["analog.stick.right.y+", state.rightStick[1]],
      ] as const;
      for (const [expression, value] of axes) {
        const directions = axisDirections[expression];
        if (!directions) continue;
        const key = `${index}\0${expression}`;
        if (Math.abs(value) < 12000) {
          axisLatch.delete(key);
          continue;
        }
        const direction = value < 0 ? -1 : 1;
        if (axisLatch.get(key) === direction) continue;
        axisLatch.set(key, direction);
        emit({ type: "move", direction: directions[direction < 0 ? 0 : 1], origin: "controller", sourceId, event });
      }
    };
    const keyboardInput = (event: KeyboardEvent) => {
      if (controllerNavigationEvents.has(event)
        || event.defaultPrevented || event.repeat || event.altKey || event.ctrlKey || event.metaKey) return;
      const directions: Partial<Record<string, InputDirection>> = {
        ArrowLeft: "left", ArrowRight: "right", ArrowUp: "up", ArrowDown: "down",
      };
      const direction = directions[event.key];
      if (direction) emit({ type: "move", direction, origin: "keyboard", sourceId: null, event });
      else if (event.key === "Enter" || event.key === " ") emit({ type: "activate", origin: "keyboard", sourceId: null, event });
      else if (event.key === "Escape") emit({ type: "back", origin: "keyboard", sourceId: null, event });
      else if (event.key === "Shift") emit({ type: "details", origin: "keyboard", sourceId: null, event });
    };
    window.Polymer.input.addEventListener("input", controllerInput);
    window.addEventListener("keydown", keyboardInput);
    onCleanup(() => {
      window.Polymer.input.removeEventListener("input", controllerInput);
      window.removeEventListener("keydown", keyboardInput);
    });
  });

  return <InputContext.Provider value={{ subscribe }}>{props.children}</InputContext.Provider>;
}
