import { onCleanup, onMount } from "solid-js";
import { dispatchControllerBack, useInputActions, type InputDirection } from "../app/InputProvider";

const focusable = "button:not(:disabled), [href], input:not(:disabled), [tabindex]:not([tabindex='-1']), [data-part='item']:not([data-disabled])";
const scopeSelector = ".horizon-input-scope";
type Vector = readonly [number, number];
const vectors: Record<InputDirection, Vector> = {
  left: [-1, 0], right: [1, 0], up: [0, -1], down: [0, 1],
};

function visible(element: HTMLElement) {
  if (element.offsetParent === null || element.closest("[hidden]")) return false;
  const closed = element.closest<HTMLElement>("[data-state='closed']");
  // Ark UI marks a Popover.Trigger as closed when its content is closed. The
  // trigger itself remains visible and navigable; only closed content and its
  // descendants should be removed from the candidate list.
  return !closed || (closed === element && element.getAttribute("data-part") === "trigger");
}

function isFocusable(element: HTMLElement) {
  if (element.localName === "button" || element.localName === "input") return !element.hasAttribute("disabled");
  if (element.localName === "a") return element.hasAttribute("href");
  const tabIndex = element.getAttribute("tabindex");
  if (tabIndex !== null) return tabIndex !== "-1";
  return element.getAttribute("data-part") === "item" && !element.hasAttribute("data-disabled");
}

function navigationScope() {
  const scopes = [...document.querySelectorAll<HTMLElement>(scopeSelector)]
    .filter((element) => element.classList.contains("horizon-input-scope") && visible(element));
  return scopes.at(-1) || document;
}

export function hasActiveInputScope() {
  return navigationScope() !== document;
}

function candidates() {
  const scope = navigationScope();
  return [...scope.querySelectorAll<HTMLElement>(focusable)].filter((element) => isFocusable(element) && visible(element));
}

function currentCandidate(items: HTMLElement[]) {
  return document.activeElement instanceof HTMLElement && items.includes(document.activeElement)
    ? document.activeElement
    : null;
}

function move(vector: Vector) {
  const items = candidates();
  if (!items.length) return;
  const current = currentCandidate(items);
  if (!current) {
    items[0].focus();
    return;
  }
  const rect = current.getBoundingClientRect();
  const origin = [rect.left + rect.width / 2, rect.top + rect.height / 2];
  const next = items.filter((candidate) => candidate !== current).map((candidate) => {
    const target = candidate.getBoundingClientRect();
    const delta = [target.left + target.width / 2 - origin[0], target.top + target.height / 2 - origin[1]];
    const primary = delta[0] * vector[0] + delta[1] * vector[1];
    const secondary = Math.abs(delta[0] * vector[1] - delta[1] * vector[0]);
    return { candidate, primary, score: primary + secondary * 2.25 };
  }).filter((item) => item.primary > 2).sort((left, right) => left.score - right.score)[0]?.candidate;
  next?.focus();
}

function activate() {
  const items = candidates();
  const target = currentCandidate(items) ?? items.find((item) => item.hasAttribute("data-input-default"));
  if (target) {
    target.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true, view: window }));
  } else {
    items[0]?.focus();
  }
}

function back() {
  const scope = navigationScope();
  const active = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const target = scope !== document && (!active || !scope.contains(active)) ? scope : active || window;
  dispatchControllerBack(target);
}

export function InputNavigation() {
  const input = useInputActions();
  onMount(() => {
    const unsubscribe = input.subscribe((action) => {
      if (navigationScope() === document && document.querySelector(".horizon-game-wall")) return;
      if (action.type === "move") {
        action.event.preventDefault();
        move(vectors[action.direction]);
      } else if (action.type === "activate") {
        action.event.preventDefault();
        activate();
      } else if (action.type === "back" && action.origin === "controller") {
        action.event.preventDefault();
        back();
      }
    });
    onCleanup(unsubscribe);
  });
  return <span class="horizon-input-navigation-anchor" hidden aria-hidden="true" />;
}
