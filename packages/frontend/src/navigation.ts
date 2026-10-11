export type Direction = "left" | "right" | "up" | "down";
export type ActionName = "move" | "activate" | "back" | "details" | "pause" | "previous" | "next";
export type InputAction = { type: ActionName; direction?: Direction; origin: "controller" | "keyboard"; sourceId: string | null; event: Event };
export type ActionHint = { action: ActionName; label: string; keyboard?: string };
export type ScopeOptions = {
  root: HTMLElement | undefined;
  enabled?: boolean;
  overlay?: boolean;
  branches?: () => readonly HTMLElement[];
  navigation?: boolean;
  initialFocus?: () => HTMLElement | null | undefined;
  returnFocus?: () => HTMLElement | null | undefined;
  hints?: () => readonly ActionHint[];
  handle?: (action: InputAction) => boolean;
};
/** Moves the focus ring from the focused element within `root` to `target`. */
export type FocusRingDelegate = { root: () => Element | undefined; target: () => Element | null | undefined };
export type NavigationChange = "revision" | "origin" | "active";

export const embeddedRuntime = "__polymerDom" in globalThis;
const focusable = "button, a[href], input, textarea, select, [tabindex]";
const vector: Record<Direction, readonly [number, number]> = { left: [-1, 0], right: [1, 0], up: [0, -1], down: [0, 1] };

/**
 * A single owner routes an action once, from the active control out to its screen.
 * Fires `revision`, `origin` and `active` events when scopes, modality or focus change.
 */
export class NavigationRouter extends EventTarget {
  private scopes: ScopeOptions[] = [];
  private delegates = new Set<FocusRingDelegate>();
  private ring: Element | null = null;
  revision = 0;
  modality: InputAction["origin"] = "keyboard";
  active: Element | null = null;

  private change(kind: NavigationChange) { this.dispatchEvent(new Event(kind)); }

  readonly focusChanged = () => {
    const active = document.activeElement;
    if (active !== this.active) { this.active = active; this.change("active"); }
    this.updateRing();
    if (!embeddedRuntime) return;
    const overlay = this.scopes.filter(scope => scope.overlay).at(-1);
    if (overlay && active && !this.contains(overlay, active)) this.items(overlay.root!)[0]?.focus();
  };
  readonly trapTab = (event: KeyboardEvent) => {
    if (event.key !== "Tab" || !embeddedRuntime) return;
    const overlay = this.scopes.filter(scope => scope.overlay).at(-1);
    if (!overlay) return;
    const items = [...this.items(overlay.root!), ...(overlay.branches?.() ?? []).flatMap(root => this.items(root))];
    const index = items.indexOf(document.activeElement as HTMLElement);
    items[(index + (event.shiftKey ? -1 : 1) + items.length) % items.length]?.focus();
    event.preventDefault();
  };
  /** Exactly one element carries data-focus-ring: the focused element, or the target of its innermost delegate. */
  readonly updateRing = () => {
    const active = document.activeElement;
    const owner = !active || active === document.body ? null : [...this.delegates]
      .filter(delegate => delegate.root()?.contains(active))
      .sort((a, b) => a.root()!.contains(b.root()!) ? 1 : -1)
      .map(delegate => delegate.target())
      .find(target => target) ?? active;
    if (owner === this.ring) return;
    this.ring?.removeAttribute("data-focus-ring");
    owner?.setAttribute("data-focus-ring", "");
    this.ring = owner;
  };

  overlayOpen() { return this.scopes.some(scope => scope.overlay); }
  activeHints() { return this.path().find(scope => scope.hints)?.hints?.() ?? []; }

  register(scope: ScopeOptions) {
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const previousRect = previous?.getBoundingClientRect();
    this.scopes.push(scope);
    this.revision++;
    this.change("revision");
    let disposed = false;
    if (scope.overlay) queueMicrotask(() => {
      if (disposed) return;
      const target = scope.initialFocus?.() ?? this.items(scope.root!)[0];
      target?.focus();
    });
    return () => {
      disposed = true;
      this.scopes = this.scopes.filter(item => item !== scope);
      this.revision++;
      this.change("revision");
      if (!scope.overlay) return;
      queueMicrotask(() => {
        const target = scope.returnFocus?.() ?? previous;
        const parent = this.scopes.filter(item => item.overlay).at(-1)?.root;
        const active = document.activeElement;
        if (active instanceof HTMLElement && active !== document.body && active.isConnected && !scope.root!.contains(active) && (!parent || parent.contains(active))) return;
        if (target?.isConnected && !target.hasAttribute("disabled") && target.getAttribute("aria-disabled") !== "true" && this.visible(target) && (!parent || parent.contains(target))) target.focus();
        else {
          const items = this.items(parent ?? document.body);
          if (previousRect) items.sort((a, b) => {
            const ar = a.getBoundingClientRect(), br = b.getBoundingClientRect();
            return Math.hypot(ar.x - previousRect.x, ar.y - previousRect.y) - Math.hypot(br.x - previousRect.x, br.y - previousRect.y);
          });
          items[0]?.focus();
        }
      });
    };
  }

  delegateRing(delegate: FocusRingDelegate) {
    this.delegates.add(delegate);
    return () => { this.delegates.delete(delegate); this.updateRing(); };
  }

  dispatch(action: InputAction) {
    if (action.event.defaultPrevented) return false;
    if (action.origin !== this.modality) { this.modality = action.origin; this.change("origin"); }
    this.focusChanged();
    for (const scope of this.path()) {
      const handled = scope.handle?.(action) || (scope.navigation && this.navigate(scope.root!, action));
      if (handled) { action.event.preventDefault(); return true; }
      // Block routing beyond a modal without cancelling the browser's native default.
      if (scope.overlay) return true;
    }
    return false;
  }

  private path() {
    const overlay = this.scopes.filter(scope => scope.overlay).at(-1);
    const active = document.activeElement === document.body ? null : document.activeElement;
    const scopes = this.scopes.filter(scope => scope.root?.isConnected && (!overlay || this.contains(overlay, scope.root)));
    const focused = scopes.filter(scope => active && this.contains(scope, active));
    focused.sort((a, b) => a.root === b.root ? Number(!!a.navigation) - Number(!!b.navigation) : a.root!.contains(b.root!) ? 1 : -1);
    if (overlay && !focused.includes(overlay)) focused.push(overlay);
    if (!focused.length) return [...scopes.filter(scope => scope.navigation && scope.root !== document.body).slice(-1), ...scopes.filter(scope => scope.root === document.body)];
    return focused;
  }

  private contains(scope: ScopeOptions, element: Element) {
    return scope.root?.contains(element) || scope.branches?.().some(root => root.contains(element));
  }

  visible(element: HTMLElement, root: HTMLElement = document.body) {
    const rect = element.getBoundingClientRect();
    const hidden = element.closest("[inert], [aria-hidden='true']");
    return !element.closest("[hidden], [data-part='content'][data-state='closed'], [data-part='positioner'][data-state='closed']") && !(hidden && root.contains(hidden)) && rect.width > 0 && rect.height > 0;
  }

  items(root: HTMLElement) {
    return [...root.querySelectorAll<HTMLElement>(focusable)].filter(item =>
      // The embedded DOM reports -1 for native controls without a tabindex.
      (item.hasAttribute("tabindex") ? Number(item.getAttribute("tabindex")) >= 0 : ["BUTTON", "INPUT", "TEXTAREA", "SELECT"].includes(item.tagName) || item.tagName === "A" && item.hasAttribute("href"))
      && !item.hasAttribute("disabled") && item.getAttribute("aria-disabled") !== "true" && !item.hasAttribute("data-disabled")
      && item.getAttribute("type") !== "hidden" && this.visible(item, root));
  }

  private navigate(root: HTMLElement, action: InputAction) {
    // Native controls and Ark composites own keyboard editing and activation.
    const active = document.activeElement as HTMLElement | null;
    if (action.origin === "keyboard" && active?.closest("input:not([type='checkbox']), textarea, select, [contenteditable='true'], [role='combobox'][aria-expanded='true'], [role='listbox'], [role='tablist'], [role='menu']")) return false;
    if (action.type === "move" && action.direction) return this.move(root, action.direction);
    if (action.type !== "activate") return false;
    if (!active || active.hasAttribute("disabled") || !(active.tagName === "BUTTON" || active.tagName === "A" && active.hasAttribute("href"))) return false;
    active.click();
    return true;
  }

  move(root: HTMLElement, direction: Direction) {
    const items = this.items(root);
    const current = items.find(item => item === document.activeElement);
    if (!current) { items[0]?.focus(); return !!items.length; }
    const rect = current.getBoundingClientRect();
    const [vx, vy] = vector[direction];
    const candidates = items.filter(item => item !== current).map(item => {
      const group = item.closest<HTMLElement>("[data-nav-group]");
      const next = (group && !group.contains(current) ? group : item).getBoundingClientRect();
      const dx = (next.left + next.right - rect.left - rect.right) / 2;
      const dy = (next.top + next.bottom - rect.top - rect.bottom) / 2;
      return { item, distance: dx * vx + dy * vy, offset: Math.abs(dx * vy - dy * vx) };
    }).filter(item => item.distance > 2).sort((a, b) => a.distance + a.offset * 2.25 - b.distance - b.offset * 2.25);
    const next = candidates[0]?.item;
    next?.focus();
    next?.scrollIntoView?.({ block: "nearest", inline: "nearest" });
    return !!next;
  }
}
