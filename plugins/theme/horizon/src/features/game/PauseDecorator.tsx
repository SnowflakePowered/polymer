import { createEffect, onCleanup, type ParentProps } from "solid-js";
import type { LaunchInstance } from "../../../../../../js/snowglass";
import { useShell } from "../../app/ShellProvider";

class PauseRegistry {
  readonly #leases = new Map<LaunchInstance, number>();

  acquire(launch: LaunchInstance) {
    const isRootLease = !this.#leases.has(launch);
    const leases = this.#leases.get(launch) ?? 0;
    this.#leases.set(launch, leases + 1);
    if (isRootLease) {
      launch.deactivateInput();
      launch.setPauseState("pause");
    }
    return () => this.#release(launch);
  }

  #release(launch: LaunchInstance) {
    const remaining = Math.max(0, (this.#leases.get(launch) ?? 1) - 1);
    this.#leases.set(launch, remaining);
    queueMicrotask(() => {
      if (this.#leases.get(launch) !== 0) return;
      this.#leases.delete(launch);
      launch.setPauseState("resume");
      launch.activateInput();
    });
  }
}

const pauseRegistry = new PauseRegistry();

/**
 * Marks UI that takes exclusive control while a game is running.
 *
 * The first overlay filters input and requests emulator pause; the final
 * overlay release resumes the emulator and restores input.
 */
export function PauseDecorator(props: ParentProps) {
  const shell = useShell();

  createEffect(() => {
    const launch = shell.model.launch;
    if (!launch) return;
    onCleanup(pauseRegistry.acquire(launch));
  });

  return <>{props.children}</>;
}
