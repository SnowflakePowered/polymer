import type { HTMLTrackedSurfaceElement, SaveState, SaveStateStore } from "./polymer";

const SCREENSHOT_TYPE = "image/jpeg";

/** Saves and presents save-state slots, shared by every theme. */
export class SaveStateSlots {
  static #urls = new WeakMap<SaveState, string>();

  /** Saves into `slot`, capturing the first tracked surface as its screenshot when one has a frame. */
  static async save(store: SaveStateStore, slot: number): Promise<SaveState> {
    const surface = document.querySelector<HTMLTrackedSurfaceElement>("tracked-surface");
    const bytes = await surface?.captureSurface(SCREENSHOT_TYPE).catch(error => {
      console.warn("Unable to capture a save-state screenshot", error);
      return undefined;
    });
    return store.save({ slot, screenshot: bytes ? { bytes, type: SCREENSHOT_TYPE } : null });
  }

  /** A data URL for the entry's screenshot, encoded once per entry. */
  static screenshot(entry: SaveState): string | undefined {
    if (!entry.screenshot) return undefined;
    let url = SaveStateSlots.#urls.get(entry);
    if (!url) {
      const { bytes, type } = entry.screenshot;
      url = `data:${type};base64,${(bytes as Uint8Array & { toBase64(): string }).toBase64()}`;
      SaveStateSlots.#urls.set(entry, url);
    }
    return url;
  }

  /** Every slot in order, paired with its entry when occupied. */
  static slots(store: SaveStateStore): { slot: number; entry: SaveState | null }[] {
    return Array.from({ length: store.capacity }, (_, slot) => ({ slot, entry: store.get(slot) }));
  }
}
