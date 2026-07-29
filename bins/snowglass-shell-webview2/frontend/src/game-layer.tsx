import {
  createElement,
  useState,
  type CSSProperties,
  type Dispatch,
  type MouseEvent as ReactMouseEvent,
  type SetStateAction,
} from "react";
import { createRoot } from "react-dom/client";
import type {
  GameViewInstance,
  HTMLTrackedSurfaceElement,
  LaunchRequest,
} from "./gameview";
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
  userPositioned: boolean;
  x: number;
  y: number;
  width: number;
  height: number;
  renderedY: number;
  interaction: GamepadInteraction | null;
  animation: GamepadAnimation | null;
};

type GamePhase = "idle" | "launching" | "running";

type SurfaceFade = {
  from: number;
  to: number;
  started: number;
};

type GameLifecycleState = {
  phase: GamePhase;
  instance: GameViewInstance | null;
  surfaceFade: number;
  fade: SurfaceFade | null;
};

const DRAWER_DURATION_MS = 260;
const SURFACE_FADE_DURATION_MS = 320;
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
    userPositioned: false,
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
  cornerRadius: number,
  style?: CSSProperties,
) {
  return createElement("tracked-surface", {
    className,
    src: String(index),
    "data-corner-radius": String(cornerRadius),
    style,
  });
}

function publishLifecycle(phase: string, status: string, error?: string) {
  postMessage({
    type: "gameViewLifecycle",
    phase,
    status,
    ...(error ? { error } : {}),
  });
  postMessage({ type: "gameViewStatus", status });
}

function launchGame(
  rawOptions: LaunchRequest,
  onSurfacesChanged: (surfaces: SurfaceDimensions[]) => void,
) {
  let instance: GameViewInstance | null;
  try {
    const surfaces = rawOptions.surfaces.map((surface) => {
      const attach = document.querySelector<HTMLTrackedSurfaceElement>(
        `tracked-surface[src="${surface.index}"]`,
      );
      if (!attach) {
        throw new Error(
          `No background tracked-surface exists for target ${surface.index}`,
        );
      }
      return {
        ...surface,
        attach,
      };
    });
    instance = window.gameView.launch({
      ...rawOptions,
      surfaces,
    });
  } catch (launchError) {
    const message =
      launchError instanceof Error ? launchError.message : String(launchError);
    bindings.setLifecycle?.((state) => ({
      ...state,
      phase: "idle",
      instance: null,
      surfaceFade: 1,
      fade: null,
    }));
    publishLifecycle("error", "Game launch failed", message);
    return;
  }
  if (!instance) {
    publishLifecycle(
      "error",
      "Game launch was rejected",
      "Another game may already be running",
    );
    return;
  }
  bindings.setLifecycle?.((state) => ({
    ...state,
    phase: "launching",
    instance,
    surfaceFade: 1,
    fade: null,
  }));
  publishLifecycle("launching", "Launching game");
  instance.on("ready", () => {
    bindings.setLifecycle?.((state) => ({ ...state, phase: "running" }));
    startSurfaceFade(0);
    publishLifecycle("running", "Game is running");
  });
  instance.on("surfacesChanged", (surfaces) => {
    onSurfacesChanged(surfaces as SurfaceDimensions[]);
  });
  instance.on("error", (error: unknown) => {
    const message =
      error && typeof error === "object" && "message" in error
        ? String(error.message)
        : null;
    const status = message || "Game launch failed";
    bindings.setLifecycle?.((state) => ({
      ...state,
      phase: "idle",
      instance: null,
      surfaceFade: 1,
      fade: null,
    }));
    publishLifecycle("error", status, status);
  });
  instance.on("closed", () => {
    bindings.setLifecycle?.((state) => ({
      ...state,
      phase: "idle",
      instance: null,
      surfaceFade: 1,
      fade: null,
    }));
    publishLifecycle("closed", "Game closed");
  });
}

type GameLayerBindings = {
  installed: boolean;
  shortcutHeld: boolean;
  interacting: boolean;
  animationTimer: ReturnType<typeof setInterval> | null;
  fadeTimer: ReturnType<typeof setInterval> | null;
  setGamepad: Dispatch<SetStateAction<GamepadState>> | null;
  setLifecycle: Dispatch<SetStateAction<GameLifecycleState>> | null;
  setMainSource: Dispatch<SetStateAction<{ width: number; height: number }>> | null;
  setViewport: Dispatch<SetStateAction<Viewport>> | null;
};

const bindings: GameLayerBindings = {
  installed: false,
  shortcutHeld: false,
  interacting: false,
  animationTimer: null,
  fadeTimer: null,
  setGamepad: null,
  setLifecycle: null,
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

function startSurfaceFade(to: number) {
  const setter = bindings.setLifecycle;
  if (!setter) return;
  if (bindings.fadeTimer !== null) clearInterval(bindings.fadeTimer);
  setter((state) => ({
    ...state,
    fade: {
      from: state.surfaceFade,
      to,
      started: Date.now(),
    },
  }));
  bindings.fadeTimer = setInterval(() => {
    bindings.setLifecycle?.((state) => {
      const fade = state.fade;
      if (!fade) return state;
      const progress = Math.min(
        1,
        (Date.now() - fade.started) / SURFACE_FADE_DURATION_MS,
      );
      const eased = 1 - (1 - progress) ** 3;
      const surfaceFade = fade.from + (fade.to - fade.from) * eased;
      if (progress < 1) return { ...state, surfaceFade };
      if (bindings.fadeTimer !== null) {
        clearInterval(bindings.fadeTimer);
        bindings.fadeTimer = null;
      }
      return { ...state, surfaceFade: fade.to, fade: null };
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
  setLifecycle: Dispatch<SetStateAction<GameLifecycleState>>,
  setMainSource: Dispatch<SetStateAction<{ width: number; height: number }>>,
  setViewport: Dispatch<SetStateAction<Viewport>>,
) {
  bindings.setGamepad = setGamepad;
  bindings.setLifecycle = setLifecycle;
  bindings.setMainSource = setMainSource;
  bindings.setViewport = setViewport;
  if (bindings.installed) return true;
  bindings.installed = true;

  addEventListener("message", (event) => {
    if (
      event.data?.type !== "snowglass-cli-launch" &&
      event.data?.type !== "snowglass-browser-launch"
    ) {
      return;
    }
    launchGame(event.data.options as LaunchRequest, (surfaces) => {
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
        return { ...state, userPositioned: true, x, y, renderedY: y };
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
      const x = state.userPositioned
        ? clamp(state.x, 0, Math.max(0, innerWidth - width))
        : Math.max(0, (innerWidth - width) / 2);
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
  const [lifecycle, setLifecycle] = useState<GameLifecycleState>({
    phase: "idle",
    instance: null,
    surfaceFade: 1,
    fade: null,
  });
  const [mainSource, setMainSource] = useState({ width: 16, height: 9 });
  const [viewport, setViewport] = useState<Viewport>({
    width: innerWidth,
    height: innerHeight,
  });
  useState(() =>
    installGameLayerBindings(
      setGamepad,
      setLifecycle,
      setMainSource,
      setViewport,
    ),
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
  const mainX = (viewport.width - mainWidth) / 2;
  const mainY = (viewport.height - mainHeight) / 2;
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
  const mainOverlayStyle: CSSProperties = {
    left: `${mainX.toFixed(1)}px`,
    top: `${mainY.toFixed(1)}px`,
    width: `${mainWidth.toFixed(1)}px`,
    height: `${mainHeight.toFixed(1)}px`,
  };

  return (
    <section className="surface-stage">
      <article className="primary-surface">
        {trackedSurface(0, "game-view main-game-view", 4, mainStyle)}
      </article>
      <div className="main-view-overlay" style={mainOverlayStyle}>
        <div
          className="main-surface-fade"
          style={{ opacity: lifecycle.surfaceFade }}
        />
      </div>
      <div
        className="gamepad-drawer"
        data-visible={gamepad.visible ? "true" : "false"}
        style={gamepadStyle}
      >
        <div
          className="gamepad-drag-handle"
          onMouseDown={beginInteraction("drag")}
        >
          <span className="gamepad-grip" />
        </div>
        {trackedSurface(1, "game-view gamepad-view", 9)}
        <div
          className="gamepad-surface-fade"
          style={{ opacity: lifecycle.surfaceFade }}
        />
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
