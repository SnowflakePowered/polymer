const files = import.meta.glob("../../../assets/prompts/keyboard-mouse/*.svg", { eager: true, query: "?url", import: "default" }) as Record<string, string>;

const NAMES: Readonly<Record<string, string>> = {
  ArrowUp: "arrow_up", ArrowDown: "arrow_down", ArrowLeft: "arrow_left", ArrowRight: "arrow_right",
  Enter: "enter", NumpadEnter: "numpad_enter", Backspace: "backspace", Space: "space", Escape: "escape", Tab: "tab",
  ShiftLeft: "shift", ShiftRight: "shift", ControlLeft: "ctrl", ControlRight: "ctrl", AltLeft: "alt", AltRight: "alt",
  MetaLeft: "win", MetaRight: "win", CapsLock: "capslock", Home: "home", End: "end", PageUp: "page_up", PageDown: "page_down",
  Insert: "insert", Delete: "delete", Comma: "comma", Period: "period", Slash: "slash_forward", Backslash: "slash_back",
  Semicolon: "semicolon", Quote: "apostrophe", BracketLeft: "bracket_open", BracketRight: "bracket_close",
  Equal: "equals", Minus: "minus", Backquote: "tilde", NumpadAdd: "numpad_plus", PrintScreen: "printscreen", Pause: "pause",
  ScrollLock: "scroll_lock", NumLock: "numlock",
};

const MOUSE: Readonly<Record<string, string>> = {
  "mouse.button.0": "mouse_left", "mouse.button.1": "mouse_scroll", "mouse.button.2": "mouse_right",
  "mouse.button.3": "mouse_side_back", "mouse.button.4": "mouse_side_forward",
  "mouse.delta.x": "mouse_horizontal", "mouse.delta.y": "mouse_vertical",
  "mouse.position.x": "mouse_horizontal", "mouse.position.y": "mouse_vertical",
};

const url = (name: string) => files[`../../../assets/prompts/keyboard-mouse/${name}.svg`];

export function keyGlyph(source: string): string | undefined {
  if (source.startsWith("mouse.")) return url(MOUSE[source] ?? "mouse");
  if (!source.startsWith("key.")) return undefined;
  const code = source.slice(4);
  const name = /^(?:Key|Digit|Numpad)([A-Z\d])$/.exec(code)?.[1].toLowerCase() ?? (/^F\d{1,2}$/.test(code) ? code.toLowerCase() : NAMES[code]);
  return name ? url(`keyboard_${name}`) : undefined;
}
