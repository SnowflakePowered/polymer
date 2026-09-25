import { createContext, createEffect, useContext, type Accessor, type ParentProps } from "solid-js";
import { palette, type ColorMode, type Tone } from "./palette";

export { palette, type ColorMode, type Tone } from "./palette";

const ColorModeContext = createContext<Accessor<ColorMode>>((): ColorMode => "light");

export function ColorModeProvider(props: ParentProps<{ mode: ColorMode }>) {
  createEffect(() => document.documentElement.setAttribute("data-color-mode", props.mode));
  return <ColorModeContext.Provider value={() => props.mode}>{props.children}</ColorModeContext.Provider>;
}

export function useColorMode() { return useContext(ColorModeContext); }

export function useTone(tone: () => Tone) {
  const mode = useColorMode();
  return () => palette[mode()][tone()];
}
