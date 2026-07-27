import {
  createElement,
  useEffect,
  useRef,
  useState,
  type CSSProperties,
} from "react";
import { createRoot } from "react-dom/client";
import {
  DraggableCore,
  type DraggableData,
  type DraggableEvent,
} from "react-draggable";
import "./gameview";

type DragBounds = {
  minX: number;
  maxX: number;
  minY: number;
  maxY: number;
};

type DragPosition = {
  x: number;
  y: number;
};

type GameLayerStyle = CSSProperties & {
  "--snowglass-drag-x": string;
  "--snowglass-drag-y": string;
};

function trackedSurface(index: number) {
  // React accepts custom element names at runtime. createElement keeps the
  // custom src attribute typed without widening all JSX intrinsic elements.
  return createElement("tracked-surface", {
    className: "game-view",
    src: String(index),
  });
}

function launchFromCli(event: MessageEvent) {
  if (event.data?.type !== "snowglass-cli-launch") return;
  const options = {
    ...event.data.options,
    surfaces: event.data.options.surfaces.map(
      (surface: { index: number; [key: string]: unknown }) => ({
        ...surface,
        attach: document.querySelector(
          `tracked-surface[src="${surface.index}"]`,
        ),
      }),
    ),
  };
  const instance = window.gameView.launch(options);
  if (!instance) {
    postMessage({ type: "gameViewStatus", status: "Game launch was rejected" });
    return;
  }
  postMessage({ type: "gameViewStatus", status: "Launching game" });
  instance.on("ready", () => {
    postMessage({ type: "gameViewStatus", status: "Game is running" });
  });
  instance.on("error", (error: unknown) => {
    const message =
      error && typeof error === "object" && "message" in error
        ? String(error.message)
        : null;
    postMessage({
      type: "gameViewStatus",
      status: message || "Game launch failed",
    });
  });
  instance.on("closed", () => {
    postMessage({ type: "gameViewStatus", status: "Game closed" });
  });
}

function GameLayer() {
  const controller = useRef<HTMLDivElement>(null);
  const dragBounds = useRef<DragBounds>({
    minX: 0,
    maxX: 0,
    minY: 0,
    maxY: 0,
  });
  const [position, setPosition] = useState<DragPosition>({ x: 0, y: 0 });

  useEffect(() => {
    addEventListener("message", launchFromCli);
    return () => removeEventListener("message", launchFromCli);
  }, []);

  const startDrag = (event: DraggableEvent) => {
    const node = controller.current;
    if (!node) return false;
    const rect = node.getBoundingClientRect();
    dragBounds.current = {
      minX: position.x - rect.left,
      maxX: position.x + innerWidth - rect.right,
      minY: position.y - rect.top,
      maxY: position.y + innerHeight - rect.bottom,
    };
    event.preventDefault();
  };

  const drag = (event: DraggableEvent, data: DraggableData) => {
    event.preventDefault();
    setPosition((current) => ({
      x: Math.max(
        dragBounds.current.minX,
        Math.min(dragBounds.current.maxX, current.x + data.deltaX),
      ),
      y: Math.max(
        dragBounds.current.minY,
        Math.min(dragBounds.current.maxY, current.y + data.deltaY),
      ),
    }));
  };

  const stopDrag = (event: DraggableEvent) => {
    event.preventDefault();
  };

  const style: GameLayerStyle = {
    "--snowglass-drag-x": `${position.x.toFixed(1)}px`,
    "--snowglass-drag-y": `${position.y.toFixed(1)}px`,
  };

  return (
    <section className="surface-stage">
      <article className="primary-surface">{trackedSurface(0)}</article>
      <DraggableCore
        nodeRef={controller}
        cancel="tracked-surface"
        enableUserSelectHack={false}
        onStart={startDrag}
        onDrag={drag}
        onStop={stopDrag}
      >
        <div
          ref={controller}
          className="drc-controller"
          data-snowglass-drag
          style={style}
        >
          <div className="drc-screen">{trackedSurface(1)}</div>
        </div>
      </DraggableCore>
    </section>
  );
}

const root = document.querySelector("#root");
if (!root) throw new Error("The game-layer React root is missing");
createRoot(root).render(<GameLayer />);
