import { createEffect, onCleanup, type ParentProps } from "solid-js";
import { PauseRegistry, type PauseRelease } from "../../../../../../js/pause";
import { useShell } from "../../app/shell-context";

const pauseRegistry = new PauseRegistry();

/** Marks UI that takes exclusive control while a game is running. */
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
