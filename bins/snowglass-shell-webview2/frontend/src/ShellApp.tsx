import {
  type PointerEvent as ReactPointerEvent,
  useEffect,
  useRef,
  useState,
} from "react";
import drcFrameUrl from "./assets/wiiu-drc.png";
import {
  type GameViewInstance,
  type LaunchOptions,
  type SerializedLaunchOptions,
  type SerializedTrackedSurface,
  subscribeToCliLaunch,
} from "./gameview";
import "./shell.css";

type TargetRef = (element: HTMLElement | null) => void;

type DragState = {
  pointerId: number;
  startX: number;
  startY: number;
  originX: number;
  originY: number;
  minX: number;
  maxX: number;
  minY: number;
  maxY: number;
};

const clamp = (value: number, minimum: number, maximum: number) =>
  Math.min(maximum, Math.max(minimum, value));

function DrcSurface({
  surface,
  targetRef,
}: {
  surface: SerializedTrackedSurface;
  targetRef: TargetRef;
}) {
  const controller = useRef<HTMLElement>(null);
  const drag = useRef<DragState | undefined>(undefined);
  const [offset, setOffset] = useState({ x: 0, y: 0 });
  const [dragging, setDragging] = useState(false);

  const beginDrag = (event: ReactPointerEvent<HTMLElement>) => {
    if (event.button !== 0 || !controller.current) return;
    const bounds = controller.current.getBoundingClientRect();
    drag.current = {
      pointerId: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
      originX: offset.x,
      originY: offset.y,
      minX: offset.x - bounds.left,
      maxX: offset.x + window.innerWidth - bounds.right,
      minY: offset.y - bounds.top,
      maxY: offset.y + window.innerHeight - bounds.bottom,
    };
    event.currentTarget.setPointerCapture(event.pointerId);
    setDragging(true);
    event.preventDefault();
  };

  const continueDrag = (event: ReactPointerEvent<HTMLElement>) => {
    const active = drag.current;
    if (!active || active.pointerId !== event.pointerId) return;
    setOffset({
      x: clamp(
        active.originX + event.clientX - active.startX,
        active.minX,
        active.maxX,
      ),
      y: clamp(
        active.originY + event.clientY - active.startY,
        active.minY,
        active.maxY,
      ),
    });
    event.preventDefault();
  };

  const endDrag = (event: ReactPointerEvent<HTMLElement>) => {
    if (drag.current?.pointerId !== event.pointerId) return;
    drag.current = undefined;
    setDragging(false);
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
  };

  const dragHandlers = {
    onPointerDown: beginDrag,
    onPointerMove: continueDrag,
    onPointerUp: endDrag,
    onPointerCancel: endDrag,
    onLostPointerCapture: endDrag,
  };

  return (
    <article
      ref={controller}
      className={`drc-controller${dragging ? " is-dragging" : ""}`}
      style={{
        transform: `translate3d(${offset.x}px, ${offset.y}px, 0)`,
      }}
      data-gameview-foreground-hits=""
    >
      <div className="drc-screen">
        <game-view-target
          id={surface.attach}
          ref={targetRef}
          data-game-view-auto-fit=""
        />
      </div>
      <img className="drc-frame-image" src={drcFrameUrl} alt="" draggable={false} />
      {(["top", "left", "right", "bottom"] as const).map((side) => (
        <span
          className={`drc-drag-region drc-drag-region-${side}`}
          data-gameview-foreground-hit=""
          aria-label="Drag the Wii U GamePad"
          key={side}
          {...dragHandlers}
        />
      ))}
      <span className="surface-label">Surface {surface.index}</span>
    </article>
  );
}

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
  const drcSurface = pendingLaunch?.surfaces[1];
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
        {drcSurface && (
          <DrcSurface
            key={drcSurface.attach}
            surface={drcSurface}
            targetRef={targetRef(drcSurface.index)}
          />
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
