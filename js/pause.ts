import type { LaunchInstance } from "./polymer";

export type PauseRelease = () => Promise<void>;

type PauseEntry = {
  leases: number;
  paused: boolean;
  sync?: Promise<void>;
};

/**
 * Reference-counted pause leases for UI that takes exclusive control while a game is running.
 * The first lease filters input and requests emulator pause; the final release resumes the
 * emulator and restores input.
 */
export class PauseRegistry {
  readonly #entries = new Map<LaunchInstance, PauseEntry>();

  acquire(launch: LaunchInstance): PauseRelease {
    let entry = this.#entries.get(launch);
    if (!entry) {
      entry = { leases: 0, paused: false };
      this.#entries.set(launch, entry);
    }
    entry.leases += 1;
    if (entry.leases === 1) {
      launch.deactivateInput();
      void this.#sync(launch, entry).catch((error) => {
        console.error("Failed to pause emulator", error);
      });
    }
    let release: Promise<void> | undefined;
    return () => {
      release ??= this.#release(launch, entry);
      return release;
    };
  }

  #release(launch: LaunchInstance, entry: PauseEntry): Promise<void> {
    entry.leases = Math.max(0, entry.leases - 1);
    // Route changes dispose one pause surface before mounting the next. Give
    // the replacement surface a turn to acquire its lease before resuming.
    return new Promise((resolve) => setTimeout(resolve, 0))
      .then(() => this.#sync(launch, entry));
  }

  #sync(launch: LaunchInstance, entry: PauseEntry): Promise<void> {
    entry.sync ??= (async () => {
      while (entry.paused !== entry.leases > 0) {
        const pause = entry.leases > 0;
        await launch.setPauseState(pause ? "pause" : "resume");
        entry.paused = pause;
      }
    })().finally(() => {
      entry.sync = undefined;
      // Closing the last overlay must restore input even if resume fails.
      if (entry.leases === 0) {
        launch.activateInput();
        if (!entry.paused) this.#entries.delete(launch);
      }
    });
    return entry.sync;
  }
}
