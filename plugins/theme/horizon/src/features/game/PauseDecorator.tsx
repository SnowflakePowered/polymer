import { createEffect, onCleanup, type ParentProps } from "solid-js";
import type { LaunchInstance } from "../../../../../../js/polymer";
import { useShell } from "../../app/shell-context";

export type PauseRelease = () => Promise<void>;

class PauseRegistry {
  readonly #leases = new Map<LaunchInstance, number>();

  acquire(launch: LaunchInstance) {
    const isRootLease = !this.#leases.has(launch);
    const leases = this.#leases.get(launch) ?? 0;
    this.#leases.set(launch, leases + 1);
    if (isRootLease) {
      launch.deactivateInput();
      void launch.setPauseState("pause").catch((error) => {
        console.error("Failed to pause emulator", error);
      });
    }
    let release: Promise<void> | undefined;
    return () => {
      release ??= this.#release(launch).catch((error) => {
        release = undefined;
        throw error;
      });
      return release;
    };
  }

  #release(launch: LaunchInstance): Promise<void> {
    const remaining = Math.max(0, (this.#leases.get(launch) ?? 1) - 1);
    this.#leases.set(launch, remaining);
    // Route changes dispose one pause surface before mounting the next. Give
    // the replacement surface a turn to acquire its lease before resuming.
    return new Promise((resolve, reject) => setTimeout(() => {
      if (this.#leases.get(launch) !== 0) {
        resolve();
        return;
      }
      launch.setPauseState("resume").finally(() => {
        // Closing the last overlay must restore input even if resume fails.
        // A new overlay may have acquired a lease while the command was pending.
        if (this.#leases.get(launch) === 0) {
          this.#leases.delete(launch);
          launch.activateInput();
        }
      }).then(resolve, reject);
    }, 0));
  }
}

const pauseRegistry = new PauseRegistry();

/**
 * Marks UI that takes exclusive control while a game is running.
 *
 * The first overlay filters input and requests emulator pause; the final
 * overlay release resumes the emulator and restores input.
 */
export function PauseDecorator(props: ParentProps<{
  releaseRef?: (release: PauseRelease) => void;
}>) {
  const shell = useShell();

  createEffect(() => {
    const launch = shell.model.launch;
    if (!launch) return;
    const release = pauseRegistry.acquire(launch);
    props.releaseRef?.(release);
    onCleanup(() => {
      void release().catch((error) => {
        console.error("Failed to resume emulator", error);
      });
    });
  });

  return <>{props.children}</>;
}
