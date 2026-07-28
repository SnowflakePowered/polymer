import {
  createElement,
  useState,
  type CSSProperties,
  type Dispatch,
  type MouseEvent as ReactMouseEvent,
  type SetStateAction,
} from "react";
import { createRoot } from "react-dom/client";
import "./gameview";

type SurfaceDimensions = {
  index: number;
  width: number;
  height: number;
};

type Viewport = {
  width: number;
  height: number;
};

type GamepadInteraction = {
  kind: "drag" | "resize";
  startX: number;
  startY: number;
  x: number;
  y: number;
  width: number;
  height: number;
};

type GamepadAnimation = {
  from: number;
  to: number;
  started: number;
};

type GamepadState = {
  visible: boolean;
  x: number;
  y: number;
  width: number;
  height: number;
  renderedY: number;
  interaction: GamepadInteraction | null;
  animation: GamepadAnimation | null;
};

const DRAWER_DURATION_MS = 260;
const DRAWER_MARGIN_PX = 24;
const DRAWER_HORIZONTAL_PADDING_PX = 10.4;
const DRAWER_HEADER_PX = 28;
const DRAWER_BOTTOM_PADDING_PX = 10.4;
const MIN_GAMEPAD_WIDTH_PX = 240;
const MAX_GAMEPAD_WIDTH_PX = 960;
const MAIN_HORIZONTAL_MARGIN_PX = 128;
const MAIN_VERTICAL_MARGIN_PX = 48;

function clamp(value: number, minimum: number, maximum: number) {
  return Math.max(minimum, Math.min(maximum, value));
}

function gamepadHeight(width: number) {
  const screenWidth = Math.max(1, width - DRAWER_HORIZONTAL_PADDING_PX * 2);
  return (
    screenWidth * (480 / 854) + DRAWER_HEADER_PX + DRAWER_BOTTOM_PADDING_PX
  );
}

function initialGamepadState(): GamepadState {
  const width = Math.max(
    1,
    Math.min(
      768,
      Math.max(352, innerWidth * 0.48),
      innerWidth - DRAWER_MARGIN_PX * 2,
    ),
  );
  const height = gamepadHeight(width);
  return {
    visible: false,
    x: Math.max(0, (innerWidth - width) / 2),
    y: Math.max(0, innerHeight - height - DRAWER_MARGIN_PX),
    width,
    height,
    renderedY: innerHeight + DRAWER_MARGIN_PX,
    interaction: null,
    animation: null,
  };
}

function trackedSurface(
  index: number,
  className: string,
  style?: CSSProperties,
) {
  return createElement("tracked-surface", {
    className,
    src: String(index),
    style,
  });
}

function launchFromCli(
  event: MessageEvent,
  onSurfacesChanged: (surfaces: SurfaceDimensions[]) => void,
) {
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
  instance.on("surfacesChanged", (surfaces) => {
    onSurfacesChanged(surfaces as SurfaceDimensions[]);
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

type GameLayerBindings = {
  installed: boolean;
  shortcutHeld: boolean;
  interacting: boolean;
  animationTimer: ReturnType<typeof setInterval> | null;
  setGamepad: Dispatch<SetStateAction<GamepadState>> | null;
  setMainSource: Dispatch<SetStateAction<{ width: number; height: number }>> | null;
  setViewport: Dispatch<SetStateAction<Viewport>> | null;
};

const bindings: GameLayerBindings = {
  installed: false,
  shortcutHeld: false,
  interacting: false,
  animationTimer: null,
  setGamepad: null,
  setMainSource: null,
  setViewport: null,
};

function startAnimationClock() {
  if (bindings.animationTimer !== null) return;
  bindings.animationTimer = setInterval(() => {
    const setter = bindings.setGamepad;
    if (!setter) return;
    setter((state) => {
      const animation = state.animation;
      if (!animation) {
        if (bindings.animationTimer !== null) {
          clearInterval(bindings.animationTimer);
          bindings.animationTimer = null;
        }
        return state;
      }
      const progress = Math.min(
        1,
        (Date.now() - animation.started) / DRAWER_DURATION_MS,
      );
      const eased = 1 - (1 - progress) ** 3;
      const renderedY =
        animation.from + (animation.to - animation.from) * eased;
      if (progress < 1) return { ...state, renderedY };
      if (bindings.animationTimer !== null) {
        clearInterval(bindings.animationTimer);
        bindings.animationTimer = null;
      }
      return { ...state, renderedY: animation.to, animation: null };
    });
  }, 16);
}

function toggleGamepad() {
  const setter = bindings.setGamepad;
  if (!setter) return;
  bindings.interacting = false;
  setter((state) => {
    const visible = !state.visible;
    return {
      ...state,
      visible,
      interaction: null,
      animation: {
        from: state.renderedY,
        to: visible ? state.y : innerHeight + DRAWER_MARGIN_PX,
        started: Date.now(),
      },
    };
  });
  startAnimationClock();
}

function installGameLayerBindings(
  setGamepad: Dispatch<SetStateAction<GamepadState>>,
  setMainSource: Dispatch<SetStateAction<{ width: number; height: number }>>,
  setViewport: Dispatch<SetStateAction<Viewport>>,
) {
  bindings.setGamepad = setGamepad;
  bindings.setMainSource = setMainSource;
  bindings.setViewport = setViewport;
  if (bindings.installed) return true;
  bindings.installed = true;

  addEventListener("message", (event) => {
    launchFromCli(event, (surfaces) => {
      const main = surfaces.find((surface) => surface.index === 0);
      if (main && main.width > 0 && main.height > 0) {
        bindings.setMainSource?.({ width: main.width, height: main.height });
      }
    });
  });

  const handleShortcut = (event: KeyboardEvent) => {
    const isEnter = event.code === "Enter" || event.code === "NumpadEnter";
    if (event.type === "keydown" && event.altKey && isEnter) {
      event.preventDefault();
      bindings.shortcutHeld = true;
      if (!event.repeat) toggleGamepad();
    } else if (event.type === "keyup" && isEnter && bindings.shortcutHeld) {
      event.preventDefault();
      bindings.shortcutHeld = false;
    }
  };
  addEventListener("keydown", handleShortcut);
  addEventListener("keyup", handleShortcut);

  addEventListener("mousemove", (event) => {
    if (!bindings.interacting) return;
    event.preventDefault();
    bindings.setGamepad?.((state) => {
      const interaction = state.interaction;
      if (!interaction) return state;
      const deltaX = event.clientX - interaction.startX;
      const deltaY = event.clientY - interaction.startY;
      if (interaction.kind === "drag") {
        const x = clamp(
          interaction.x + deltaX,
          0,
          Math.max(0, innerWidth - state.width),
        );
        const y = clamp(
          interaction.y + deltaY,
          0,
          Math.max(0, innerHeight - state.height),
        );
        return { ...state, x, y, renderedY: y };
      }

      const availableWidth = Math.max(1, innerWidth - interaction.x);
      const availableHeight = Math.max(1, innerHeight - interaction.y);
      const widthFromHeight =
        Math.max(
          1,
          availableHeight - DRAWER_HEADER_PX - DRAWER_BOTTOM_PADDING_PX,
        ) *
          (854 / 480) +
        DRAWER_HORIZONTAL_PADDING_PX * 2;
      const maximum = Math.max(
        1,
        Math.min(MAX_GAMEPAD_WIDTH_PX, availableWidth, widthFromHeight),
      );
      const minimum = Math.min(MIN_GAMEPAD_WIDTH_PX, maximum);
      const aspectSlope = 480 / 854;
      const projectedDelta =
        (deltaX + aspectSlope * deltaY) / (1 + aspectSlope ** 2);
      const width = clamp(
        interaction.width + projectedDelta,
        minimum,
        maximum,
      );
      return { ...state, width, height: gamepadHeight(width) };
    });
  });

  addEventListener("mouseup", (event) => {
    if (!bindings.interacting) return;
    event.preventDefault();
    bindings.interacting = false;
    bindings.setGamepad?.((state) => ({ ...state, interaction: null }));
  });

  addEventListener("resize", () => {
    bindings.interacting = false;
    bindings.setViewport?.({ width: innerWidth, height: innerHeight });
    bindings.setGamepad?.((state) => {
      const width = Math.min(
        state.width,
        Math.max(1, innerWidth - DRAWER_MARGIN_PX * 2),
      );
      const height = gamepadHeight(width);
      const x = clamp(state.x, 0, Math.max(0, innerWidth - width));
      const y = clamp(state.y, 0, Math.max(0, innerHeight - height));
      return {
        ...state,
        x,
        y,
        width,
        height,
        renderedY: state.visible ? y : innerHeight + DRAWER_MARGIN_PX,
        interaction: null,
        animation: null,
      };
    });
  });

  return true;
}

function GameLayer() {
  const [gamepad, setGamepad] = useState(initialGamepadState);
  const [mainSource, setMainSource] = useState({ width: 16, height: 9 });
  const [viewport, setViewport] = useState<Viewport>({
    width: innerWidth,
    height: innerHeight,
  });
  useState(() =>
    installGameLayerBindings(setGamepad, setMainSource, setViewport),
  );

  const beginInteraction =
    (kind: GamepadInteraction["kind"]) =>
    (event: ReactMouseEvent<HTMLElement>) => {
      if (event.button !== 0 || !gamepad.visible) return;
      event.preventDefault();
      event.stopPropagation();
      if (bindings.animationTimer !== null) {
        clearInterval(bindings.animationTimer);
        bindings.animationTimer = null;
      }
      bindings.interacting = true;
      setGamepad((state) => ({
        ...state,
        animation: null,
        renderedY: state.y,
        interaction: {
          kind,
          startX: event.clientX,
          startY: event.clientY,
          x: state.x,
          y: state.y,
          width: state.width,
          height: state.height,
        },
      }));
    };

  const availableWidth = Math.max(1, viewport.width - MAIN_HORIZONTAL_MARGIN_PX);
  const availableHeight = Math.max(1, viewport.height - MAIN_VERTICAL_MARGIN_PX);
  const sourceRatio = mainSource.width / mainSource.height;
  const mainWidth = Math.min(availableWidth, availableHeight * sourceRatio);
  const mainHeight = mainWidth / sourceRatio;
  const gamepadStyle: CSSProperties = {
    left: `${gamepad.x.toFixed(1)}px`,
    top: `${gamepad.renderedY.toFixed(1)}px`,
    width: `${gamepad.width.toFixed(1)}px`,
    height: `${gamepad.height.toFixed(1)}px`,
  };
  const mainStyle: CSSProperties = {
    width: `${mainWidth.toFixed(1)}px`,
    height: `${mainHeight.toFixed(1)}px`,
  };

  return (
    <section className="surface-stage">
      <article className="primary-surface">
        {trackedSurface(0, "game-view main-game-view", mainStyle)}
      </article>
      <div
        className="gamepad-drawer"
        data-visible={gamepad.visible ? "true" : "false"}
        style={gamepadStyle}
      >
        <div
          className="gamepad-drag-handle"
          onMouseDown={beginInteraction("drag")}
        />
        {trackedSurface(1, "game-view gamepad-view")}
        <div
          className="gamepad-resize-handle"
          onMouseDown={beginInteraction("resize")}
        />
      </div>
    </section>
  );
}

const root = document.querySelector("#root");
if (!root) throw new Error("The game-layer React root is missing");
createRoot(root).render(<GameLayer />);
