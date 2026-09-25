import { createEffect, createSignal } from "solid-js";
import type { FieldProps } from "../forms";
import type { InputAction } from "../input";

export type ChoiceItem = { value: string; label: string; description?: string; disabled?: boolean; group?: string };
export type ChoiceProps = FieldProps & { items: readonly ChoiceItem[]; placeholder?: string; loading?: boolean; loadError?: string };
export type SingleChoiceProps = ChoiceProps & { value: string | null; onValueChange(value: string | null): void };
export type MultipleChoiceProps = ChoiceProps & { value: readonly string[]; onValueChange(value: string[]): void };
export type PickerApi = { setHighlightValue(value: string): void; selectValue(value: string): void; highlightedValue: string | null };

/** Transaction state is separate from Ark's selection and highlighted option. */
export class ChoiceSession {
  private opened = createSignal(false);
  private selected = createSignal<string[]>([]);
  private search = createSignal("");
  private snapshot = "";
  readonly open = this.opened[0];
  readonly draft = this.selected[0];
  readonly query = this.search[0];
  readonly setQuery = this.search[1];
  constructor(private values: () => readonly string[], private multiple: () => boolean, private commit: (values: string[]) => void) {
    createEffect(() => {
      const current = JSON.stringify(values());
      if (this.open() && current !== this.snapshot) this.close();
    });
  }
  begin() {
    if (this.open()) return;
    const values = [...this.values()];
    this.snapshot = JSON.stringify(values);
    this.selected[1](values);
    this.opened[1](true);
  }
  close = () => { this.opened[1](false); this.search[1](""); };
  change = (values: string[]) => {
    this.selected[1](values);
    if (!this.multiple()) { this.close(); this.commit(values); }
  };
  apply = () => { const values = [...this.draft()]; this.close(); this.commit(values); };
  readonly value = () => this.open() ? this.draft() : [...this.values()];

  navigate(action: InputAction, api: PickerApi, items: readonly ChoiceItem[], footer?: HTMLElement) {
    if (action.type === "back") { this.close(); return true; }
    const inFooter = !!footer?.contains(document.activeElement);
    if (action.type === "activate" && !inFooter) {
      if (api.highlightedValue) api.selectValue(api.highlightedValue);
      return true;
    }
    const movement = action.type === "move" && (action.direction === "up" || action.direction === "down");
    if (!movement && action.type !== "previous" && action.type !== "next") return false;
    const enabled = items.filter(item => !item.disabled);
    const step = action.type === "previous" ? -5 : action.type === "next" ? 5 : action.direction === "up" ? -1 : 1;
    const index = enabled.findIndex(item => item.value === api.highlightedValue);
    if (step > 0 && index === enabled.length - 1 && footer && !inFooter) { footer.querySelector<HTMLButtonElement>("button")?.focus(); return true; }
    if (inFooter && step > 0) return false;
    const item = enabled[Math.max(0, Math.min(enabled.length - 1, inFooter ? enabled.length - 1 : index + step))];
    if (item) api.setHighlightValue(item.value);
    return true;
  }
}
