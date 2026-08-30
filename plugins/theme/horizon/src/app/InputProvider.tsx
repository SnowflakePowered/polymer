import { createContext, onCleanup, onMount, useContext, type ParentProps } from "solid-js";
import type { InputControlEvent } from "../../../../../js/snowglass";

export type InputDirection = "left" | "right" | "up" | "down";
export type InputAction =
  | { type: "move"; direction: InputDirection; sourceId: string | null; event: Event }
  | { type: "activate" | "back" | "details" | "pause"; sourceId: string | null; event: Event };

type ActionListener = (action: InputAction) => void;
type InputContextValue = { subscribe(listener: ActionListener): () => void };
type ControllerAction =
  | { type: "move"; direction: InputDirection }
  | { type: "activate" | "back" | "details" | "pause" };

const InputContext = createContext<InputContextValue>();
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
    const controllerInput = (event: Event) => {
      const input = (event as CustomEvent<InputControlEvent>).detail;
      const directions = axisDirections[input.expression];
      if (directions) {
        const key = `${input.sourceId}\0${input.expression}`;
        if (Math.abs(input.value) < 12000) {
          axisLatch.delete(key);
          return;
        }
        const direction = input.value < 0 ? -1 : 1;
        if (axisLatch.get(key) === direction) return;
        axisLatch.set(key, direction);
        emit({ type: "move", direction: directions[direction < 0 ? 0 : 1], sourceId: input.sourceId, event });
        return;
      }
      if (input.value !== 1) return;
      const action = buttonActions[input.expression];
      if (action) emit({ ...action, sourceId: input.sourceId, event } as InputAction);
    };
    const keyboardInput = (event: KeyboardEvent) => {
      if (controllerNavigationEvents.has(event)
        || event.defaultPrevented || event.repeat || event.altKey || event.ctrlKey || event.metaKey) return;
      const directions: Partial<Record<string, InputDirection>> = {
        ArrowLeft: "left", ArrowRight: "right", ArrowUp: "up", ArrowDown: "down",
      };
      const direction = directions[event.key];
      if (direction) emit({ type: "move", direction, sourceId: null, event });
      else if (event.key === "Enter" || event.key === " ") emit({ type: "activate", sourceId: null, event });
      else if (event.key === "Escape") emit({ type: "back", sourceId: null, event });
      else if (event.key === "Shift") emit({ type: "details", sourceId: null, event });
    };
    window.Snowglass.input.addEventListener("controllerinput", controllerInput);
    window.addEventListener("keydown", keyboardInput);
    onCleanup(() => {
      window.Snowglass.input.removeEventListener("controllerinput", controllerInput);
      window.removeEventListener("keydown", keyboardInput);
    });
  });

  return <InputContext.Provider value={{ subscribe }}>{props.children}</InputContext.Provider>;
}
