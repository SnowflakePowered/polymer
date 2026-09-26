import { op_ui_automation_next_message_box, op_ui_automation_request } from "ext:core/ops";

const DEFAULT_TIMEOUT = 5_000;

export class UiAutomationError extends Error {
  constructor(error) {
    super(error.message);
    this.name = "UiAutomationError";
    this.kind = error.kind;
    this.diagnosis = error.diagnosis;
  }
}

async function request(value) {
  const response = await op_ui_automation_request(value);
  if (!response.ok) throw new UiAutomationError(response.error);
  return response.value;
}

function value(response, expected) {
  if (response.type !== expected) {
    throw new TypeError(`UI automation returned ${response.type}, expected ${expected}`);
  }
  return response.value;
}

class UiKeyboard {
  #window;
  constructor(window) { this.#window = window; }
  async #send(command) {
    await request({ type: "input", window: this.#window, command });
  }
  down(code) { return this.#send({ kind: "keyDown", code }); }
  up(code) { return this.#send({ kind: "keyUp", code }); }
  press(code, options = {}) {
    return this.#send({
      kind: "keyPress",
      code,
      modifiers: options.modifiers ?? [],
      repeat: options.repeat ?? false,
    });
  }
  typeText(text) { return this.#send({ kind: "typeText", text }); }
}

class UiMouse {
  #window;
  constructor(window) { this.#window = window; }
  async #send(command) {
    await request({ type: "input", window: this.#window, command });
  }
  move(x, y) { return this.#send({ kind: "mouseMove", x, y }); }
  down(button, x, y) { return this.#send({ kind: "mouseDown", button, x, y }); }
  up(button, x, y) { return this.#send({ kind: "mouseUp", button, x, y }); }
  click(x, y, options = {}) {
    return this.#send({
      kind: "mouseClick",
      button: options.button ?? "left",
      count: options.count ?? 1,
      x,
      y,
    });
  }
  wheel(deltaX, deltaY) {
    return this.#send({ kind: "mouseWheel", deltaX, deltaY });
  }
}

class UiLocator {
  #window;
  #selector;
  #nth;
  #timeout;
  constructor(window, selector, nth = null, timeout = DEFAULT_TIMEOUT) {
    this.#window = window;
    this.#selector = selector;
    this.#nth = nth;
    this.#timeout = timeout;
  }
  first() { return this.nth(1); }
  nth(index) {
    if (!Number.isInteger(index) || index < 1) throw new TypeError("nth is 1-based");
    return new UiLocator(this.#window, this.#selector, index, this.#timeout);
  }
  child(selector) {
    return new UiLocator(this.#window, `${this.#selector} > ${selector}`, null, this.#timeout);
  }
  descendant(selector) {
    return new UiLocator(this.#window, `${this.#selector} ${selector}`, null, this.#timeout);
  }
  withTimeout(timeoutMs) {
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 0) {
      throw new TypeError("timeout must be a non-negative safe integer");
    }
    return new UiLocator(this.#window, this.#selector, this.#nth, timeoutMs);
  }
  async count() {
    return value(await request({ type: "locatorCount", window: this.#window,
      selector: this.#selector }), "count");
  }
  async all(options = {}) {
    return value(await request({ type: "locatorAll", window: this.#window,
      selector: this.#selector, limit: options.limit ?? 100 }), "nodes");
  }
  snapshot() { return this.waitAttached(); }
  async #wait(condition) {
    const response = await request({ type: "locatorWait", window: this.#window,
      selector: this.#selector, nth: this.#nth, condition, timeoutMs: this.#timeout });
    return response.type === "unit" ? undefined : value(response, "node");
  }
  waitAttached() { return this.#wait("attached"); }
  waitDetached() { return this.#wait("detached"); }
  waitVisible() { return this.#wait("visible"); }
  waitHidden() { return this.#wait("hidden"); }
  waitEnabled() { return this.#wait("enabled"); }
  waitFocused() { return this.#wait("focused"); }
  async #clickNode(node, options = {}) {
    if (node.bounds == null) {
      throw new UiAutomationError({
        kind: "notInteractable",
        message: `selector '${this.#selector}' has no screen bounds`,
        diagnosis: { selector: this.#selector },
      });
    }
    await request({
      type: "input",
      window: this.#window,
      command: {
        kind: "mouseClick",
        button: options.button ?? "left",
        count: options.count ?? 1,
        x: Math.round(node.bounds.x + node.bounds.width / 2),
        y: Math.round(node.bounds.y + node.bounds.height / 2),
      },
    });
  }
  async click(options = {}) {
    await this.#clickNode(await this.snapshot(), options);
  }
  async selectOption(name) {
    if (typeof name !== "string") throw new TypeError("option name must be a string");
    const control = await this.snapshot();
    // Qt on macOS opens popup buttons through AXPress and exposes options as static text.
    const axPopup = ["AXPopUpButton", "AXComboBox"].includes(control.raw?.ax_role);
    if (axPopup) await this.press();
    else await this.expand();
    const optionSelector = axPopup ? "list > static_text, menu_item" : "list_item";
    const started = Date.now();
    let matches = [];
    let lastLookupError;
    do {
      matches = [];
      // Native combo popups (including Qt's) are often separate top-level
      // windows rather than descendants of the combo box in the UIA tree.
      const windows = value(await request({
        type: "windows",
        query: {},
      }), "windows");
      const candidates = windows
        .filter(snapshot => snapshot.visible)
        .sort((left, right) =>
          Number(left.id === this.#window) - Number(right.id === this.#window));
      for (const snapshot of candidates) {
        const window = new UiWindow(snapshot);
        const options = window.locator(optionSelector);
        let items;
        try {
          items = await options.all({ limit: 100 });
        } catch (error) {
          if (!(error instanceof UiAutomationError)) throw error;
          lastLookupError = error;
          continue;
        }
        for (const option of items) {
          if (option.name === name
            && option.bounds != null
            && option.states.visible !== false
            && option.states.enabled !== false) {
            matches.push({ window, windowSnapshot: snapshot, option });
          }
        }
        if (matches.length !== 0) break;
      }
      const unique = new Map();
      for (const match of matches) {
        const bounds = match.option.bounds;
        const identity = bounds == null
          ? `${match.window.id}:${match.option.stableId ?? ""}`
          : `${match.option.processId}:${bounds.x}:${bounds.y}:${bounds.width}:${bounds.height}`;
        if (!unique.has(identity)) unique.set(identity, match);
      }
      matches = [...unique.values()];
      if (matches.length !== 0) break;
      await new Promise(resolve => setTimeout(resolve, 50));
    } while (Date.now() - started < this.#timeout);
    if (matches.length === 0) {
      if (lastLookupError != null) throw lastLookupError;
      throw new UiAutomationError({
        kind: "selectorNotMatched",
        message: `option ${JSON.stringify(name)} was not found`,
        diagnosis: { selector: this.#selector, condition: `option named ${JSON.stringify(name)}` },
      });
    }
    if (matches.length > 1) {
      throw new UiAutomationError({
        kind: "ambiguousSelector",
        message: `option ${JSON.stringify(name)} matched ${matches.length} elements`,
        diagnosis: {
          selector: this.#selector,
          lastObserved: `${matches.length} matching options`,
          candidates: matches.map(match =>
            `${match.windowSnapshot.title} (${match.windowSnapshot.className})`),
        },
      });
    }
    const match = matches[0];
    const option = match.window.locator(optionSelector);
    await option.#clickNode(match.option);

    const selected = Date.now();
    let snapshot;
    do {
      snapshot = await this.snapshot();
      if (snapshot.value === name || snapshot.name === name) return;
      await new Promise(resolve => setTimeout(resolve, 50));
    } while (Date.now() - selected < this.#timeout);
    throw new UiAutomationError({
      kind: "timeout",
      message: `option ${JSON.stringify(name)} was clicked but the value did not update`,
      diagnosis: {
        selector: this.#selector,
        condition: `value equals ${JSON.stringify(name)}`,
        lastObserved: JSON.stringify(snapshot?.value),
      },
    });
  }
  async #action(action) {
    await request({ type: "locatorAction", window: this.#window,
      selector: this.#selector, nth: this.#nth, action, timeoutMs: this.#timeout });
  }
  press() { return this.#action("press"); }
  focus() { return this.#action("focus"); }
  blur() { return this.#action("blur"); }
  toggle() { return this.#action("toggle"); }
  select() { return this.#action("select"); }
  expand() { return this.#action("expand"); }
  collapse() { return this.#action("collapse"); }
  showMenu() { return this.#action("showMenu"); }
  increment() { return this.#action("increment"); }
  decrement() { return this.#action("decrement"); }
  scrollIntoView() { return this.#action("scrollIntoView"); }
  setValue(value) { return this.#action({ setValue: value }); }
  setNumericValue(value) { return this.#action({ setNumericValue: value }); }
  typeText(value) { return this.#action({ typeText: value }); }
  setTextSelection(start, end) { return this.#action({ setTextSelection: { start, end } }); }
  perform(action) { return this.#action({ perform: action }); }
}

class UiEventStream {
  #subscription;
  #closed = false;
  constructor(subscription) { this.#subscription = subscription; }
  [Symbol.asyncIterator]() { return this; }
  async #poll(timeoutMs) {
    if (this.#closed) return null;
    const response = await request({ type: "nextEvent", subscription: this.#subscription,
      timeoutMs });
    const event = value(response, "event");
    return event == null ? null : event;
  }
  async next() {
    while (!this.#closed) {
      const event = await this.#poll(DEFAULT_TIMEOUT);
      if (event != null) return { done: false, value: event };
    }
    return { done: true, value: undefined };
  }
  poll(options = {}) {
    return this.#poll(options.timeoutMs ?? DEFAULT_TIMEOUT);
  }
  async close() {
    if (this.#closed) return;
    this.#closed = true;
    await request({ type: "closeEvents", subscription: this.#subscription });
  }
}

class UiWindow {
  #snapshot;
  constructor(snapshot) {
    this.#snapshot = snapshot;
    this.id = snapshot.id;
    this.keyboard = Object.freeze(new UiKeyboard(this.id));
    this.mouse = Object.freeze(new UiMouse(this.id));
  }
  async snapshot() {
    return value(await request({ type: "windowSnapshot", window: this.id }), "window");
  }
  async tree(options = {}) {
    return value(await request({ type: "tree", window: this.id, options }), "tree");
  }
  #renderTree(node, depth, output) {
    let line = `${"  ".repeat(depth)}${node.role}`;
    if (node.name != null) line += ` "${node.name}"`;
    if (node.value != null) line += ` value="${node.value}"`;
    output.push(`${line}\n`);
    for (const child of node.children ?? []) this.#renderTree(child, depth + 1, output);
  }
  async dump(options = {}) {
    const tree = await this.tree(options);
    const output = [];
    this.#renderTree(tree.root, 0, output);
    if (tree.truncated) output.push("\u2026 (truncated)\n");
    return output.join("");
  }
  locator(selector) { return new UiLocator(this.id, selector); }
  async events(options = {}) {
    const id = value(await request({ type: "subscribe", window: this.id,
      filter: { types: options.types ?? [] } }), "subscription");
    return new UiEventStream(id);
  }
  async setLogicalFocus() {
    await request({ type: "input", window: this.id, command: { kind: "logicalFocus" } });
  }
}

export const uiAutomation = Object.freeze({
  async windows(query = {}) {
    const windows = value(await request({ type: "windows", query }), "windows");
    return Object.freeze(windows.map(window => Object.freeze(new UiWindow(window))));
  },
  async waitForWindow(query, options = {}) {
    if (typeof query?.pattern !== "string") {
      throw new TypeError("waitForWindow requires a regex string in query.pattern");
    }
    if (query.selector !== undefined && typeof query.selector !== "string") {
      throw new TypeError("window query.selector must be an accessibility selector string");
    }
    const window = value(await request({ type: "waitForWindow", query,
      timeoutMs: options.timeoutMs ?? DEFAULT_TIMEOUT }), "window");
    return Object.freeze(new UiWindow(window));
  },
  async *messageBoxes() {
    for (;;) {
      const response = await op_ui_automation_next_message_box();
      if (!response.ok) throw new UiAutomationError(response.error);
      if (response.value == null) return;
      yield response.value;
    }
  },
});

Object.defineProperty(globalThis, "__polymerUiAutomation", { value: uiAutomation });

Object.defineProperty(globalThis, "__polymerUiAutomationError", { value: UiAutomationError });
