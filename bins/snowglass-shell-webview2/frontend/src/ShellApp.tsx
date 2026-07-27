import { useEffect, useRef, useState } from "react";
import {
  type GameViewInstance,
  type LaunchOptions,
  type SerializedLaunchOptions,
  type SerializedTrackedSurface,
  subscribeToCliLaunch,
} from "./gameview";
import "./shell.css";

export function ShellApp() {
  const [pendingLaunch, setPendingLaunch] = useState<SerializedLaunchOptions>();
  const [targetRevision, setTargetRevision] = useState(0);
  const [status, setStatus] = useState("Waiting for launch options");
  const instance = useRef<GameViewInstance | undefined>(undefined);
  const launched = useRef<SerializedLaunchOptions | undefined>(undefined);
  const targets = useRef(new Map<number, HTMLElement>());
  const targetCallbacks = useRef(
    new Map<number, (element: HTMLElement | null) => void>(),
  );

  useEffect(
    () =>
      subscribeToCliLaunch((options) => {
        targets.current.clear();
        launched.current = undefined;
        setPendingLaunch(options);
        setStatus("Preparing render targets");
      }),
    [],
  );

  useEffect(() => {
    if (!pendingLaunch || launched.current === pendingLaunch) return;
    if (
      pendingLaunch.surfaces.some(
        (surface) => !targets.current.has(surface.index),
      )
    ) {
      return;
    }
    launched.current = pendingLaunch;
    const options: LaunchOptions = {
      ...pendingLaunch,
      surfaces: pendingLaunch.surfaces.map((surface) => ({
        ...surface,
        attach: targets.current.get(surface.index)!,
      })),
    };
    const launchedInstance = window.gameView.launch(options);
    if (!launchedInstance) {
      setStatus("Game launch was rejected");
      return;
    }
    instance.current = launchedInstance;
    setStatus("Launching game");
    launchedInstance.on("ready", () => setStatus("Game is running"));
    launchedInstance.on("error", (event) => {
      const message =
        typeof event === "object" &&
        event !== null &&
        "message" in event &&
        typeof event.message === "string"
          ? event.message
          : "Game launch failed";
      setStatus(message);
    });
    launchedInstance.on("closed", () => {
      instance.current = undefined;
      setStatus("Game closed");
    });
  }, [pendingLaunch, targetRevision]);

  const targetRef = (index: number) => {
    let callback = targetCallbacks.current.get(index);
    if (!callback) {
      callback = (element) => {
        const current = targets.current.get(index);
        if (element && current !== element) {
          targets.current.set(index, element);
          setTargetRevision((revision) => revision + 1);
        } else if (!element && current) {
          targets.current.delete(index);
          setTargetRevision((revision) => revision + 1);
        }
      };
      targetCallbacks.current.set(index, callback);
    }
    return callback;
  };

  const primarySurface = pendingLaunch?.surfaces[0];
  // Surface 1 is the handheld. The game layer lays it out, draws its bezel and
  // handles dragging it, so the foreground draws nothing for it — but a stream
  // still attaches to an element, and the launch waits until every surface has
  // one, so it keeps an empty target.
  const handheldSurface = pendingLaunch?.surfaces[1];
  const extraSurfaces = pendingLaunch?.surfaces.slice(2) ?? [];

  return (
    <main className="shell">
      <section className="surface-stage">
        {primarySurface && (
          <article className="primary-surface" key={primarySurface.attach}>
            <game-view-target
              id={primarySurface.attach}
              ref={targetRef(primarySurface.index)}
              data-game-view-auto-fit=""
            />
            <span className="surface-label">Surface {primarySurface.index}</span>
          </article>
        )}
        {handheldSurface && (
          <div className="layer-owned-surface" key={handheldSurface.attach}>
            <game-view-target
              id={handheldSurface.attach}
              ref={targetRef(handheldSurface.index)}
            />
          </div>
        )}
        {extraSurfaces.length > 0 && (
          <aside className="extra-surfaces">
            {extraSurfaces.map((surface) => (
              <article className="extra-surface" key={surface.attach}>
                <game-view-target
                  id={surface.attach}
                  ref={targetRef(surface.index)}
                  data-game-view-auto-fit=""
                />
                <span className="surface-label">Surface {surface.index}</span>
              </article>
            ))}
          </aside>
        )}
      </section>
      <output className="status">{status}</output>
    </main>
  );
}
