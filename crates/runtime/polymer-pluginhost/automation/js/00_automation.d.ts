export interface WindowQuery {
  /** Rust-regex pattern matched against the window title. */
  pattern?: string;
  /** Accessibility selector that must match within the window. */
  selector?: string;
}

export interface UiNodeSnapshot {
  role: string;
  name: string | null;
  value: string | null;
  description: string | null;
  bounds: { x: number; y: number; width: number; height: number } | null;
  actions: readonly string[];
  states: Readonly<Record<string, unknown>>;
  numericValue: number | null;
  minValue: number | null;
  maxValue: number | null;
  stableId: string | null;
  processId: number | null;
  raw: Readonly<Record<string, unknown>>;
  children?: readonly UiNodeSnapshot[];
}

export interface UiWindowSnapshot {
  id: string;
  processId: number;
  title: string;
  className: string;
  visible: boolean;
}

export interface UiEvent {
  eventType: string;
  target: UiNodeSnapshot | null;
  state: string | null;
  value: boolean | null;
}

export interface UiTreeSnapshot {
  root: UiNodeSnapshot;
  truncated: boolean;
}

export interface UiLocator {
  first(): UiLocator;
  nth(index: number): UiLocator;
  child(selector: string): UiLocator;
  descendant(selector: string): UiLocator;
  withTimeout(timeoutMs: number): UiLocator;
  count(): Promise<number>;
  all(options?: { limit?: number }): Promise<readonly UiNodeSnapshot[]>;
  snapshot(): Promise<UiNodeSnapshot>;
  waitAttached(): Promise<UiNodeSnapshot>;
  waitDetached(): Promise<void>;
  waitVisible(): Promise<UiNodeSnapshot>;
  waitHidden(): Promise<void>;
  waitEnabled(): Promise<UiNodeSnapshot>;
  waitFocused(): Promise<UiNodeSnapshot>;
  click(options?: { button?: string; count?: number }): Promise<void>;
  selectOption(name: string): Promise<void>;
  press(): Promise<void>;
  focus(): Promise<void>;
  blur(): Promise<void>;
  toggle(): Promise<void>;
  select(): Promise<void>;
  expand(): Promise<void>;
  collapse(): Promise<void>;
  showMenu(): Promise<void>;
  increment(): Promise<void>;
  decrement(): Promise<void>;
  scrollIntoView(): Promise<void>;
  setValue(value: string): Promise<void>;
  setNumericValue(value: number): Promise<void>;
  typeText(value: string): Promise<void>;
  setTextSelection(start: number, end: number): Promise<void>;
  perform(action: string): Promise<void>;
}

export interface UiEventStream extends AsyncIterable<UiEvent> {
  next(): Promise<IteratorResult<UiEvent>>;
  poll(options?: { timeoutMs?: number }): Promise<UiEvent | null>;
  close(): Promise<void>;
}

export interface UiWindow {
  readonly id: string;
  readonly keyboard: {
    down(code: string): Promise<void>;
    up(code: string): Promise<void>;
    press(code: string, options?: { modifiers?: readonly string[]; repeat?: boolean }): Promise<void>;
    typeText(text: string): Promise<void>;
  };
  readonly mouse: {
    move(screenX: number, screenY: number): Promise<void>;
    down(button: string, screenX: number, screenY: number): Promise<void>;
    up(button: string, screenX: number, screenY: number): Promise<void>;
    click(screenX: number, screenY: number, options?: { button?: string; count?: number }): Promise<void>;
    wheel(deltaX: number, deltaY: number): Promise<void>;
  };
  snapshot(): Promise<UiWindowSnapshot>;
  tree(options?: { maxDepth?: number; maxNodes?: number }): Promise<UiTreeSnapshot>;
  dump(options?: { maxDepth?: number; maxNodes?: number }): Promise<string>;
  locator(selector: string): UiLocator;
  events(options?: { types?: readonly string[] }): Promise<UiEventStream>;
  setLogicalFocus(): Promise<void>;
}

export interface UiAutomation {
  windows(query?: WindowQuery): Promise<readonly UiWindow[]>;
  waitForWindow(query: WindowQuery, options?: { timeoutMs?: number }): Promise<UiWindow>;
}

export class UiAutomationError extends Error {
  readonly kind: string;
  readonly diagnosis?: Readonly<Record<string, unknown>>;
}

export const uiAutomation: UiAutomation;
