import type { DetailedHTMLProps, HTMLAttributes } from "react";

export type GraphicsHook = "opengl" | "vulkan" | "d3d11" | "d3d12";

export type TrackedSurface = {
  index: number;
  rotation: number;
  selector?: string;
  attach: HTMLElement;
  webrtc: boolean;
};

export type SerializedTrackedSurface = Omit<TrackedSurface, "attach"> & {
  attach: string;
};

export type GamePadMapping = {
  gamepadId: number | "K" | string;
  mouse: boolean;
  slot: number;
  layout: string;
  mapping: Record<string, string>;
};

export type LaunchOptions = {
  command: string;
  arguments: string[];
  isolated: boolean;
  hook: GraphicsHook[];
  surfaces: TrackedSurface[];
  gamepadMapping: GamePadMapping[];
  webrtcBind?: string;
};

export type SerializedLaunchOptions = Omit<LaunchOptions, "surfaces"> & {
  surfaces: SerializedTrackedSurface[];
};

export interface GameViewInstance {
  on(eventName: string, callback: (event: unknown) => void): void;
  readonly surfaces?: TrackedSurface[];
  message(messageName: string, args: unknown): void;
  close(): void;
}

export interface GameViewManager {
  launch(options: LaunchOptions): GameViewInstance | null;
  readonly activeInstance?: GameViewInstance;
  activateInput(): void;
  deactivateInput(): void;
}

export interface GameViewTargetElement extends HTMLElement {
  readonly preferredSize: Readonly<{ width: number; height: number }>;
  scaling: "integer" | "fit";
  integerScaling: boolean;
}

declare global {
  interface Window {
    gameView: GameViewManager;
    snowglassGameView: GameViewManager;
    readonly snowglassCliLaunchOptions?: SerializedLaunchOptions;
  }

  interface WindowEventMap {
    snowglassclilaunch: CustomEvent<SerializedLaunchOptions>;
  }

  interface HTMLElementTagNameMap {
    "game-view-target": GameViewTargetElement;
  }
}

declare module "react" {
  namespace JSX {
    interface IntrinsicElements {
      "game-view-target": DetailedHTMLProps<HTMLAttributes<HTMLElement>, HTMLElement> & {
        scaling?: "integer" | "fit";
      };
    }
  }
}

export function subscribeToCliLaunch(
  callback: (options: SerializedLaunchOptions) => void,
): () => void {
  let delivered: SerializedLaunchOptions | undefined;
  const deliver = (options: SerializedLaunchOptions) => {
    if (options === delivered) return;
    delivered = options;
    callback(options);
  };
  const listener = (event: WindowEventMap["snowglassclilaunch"]) => deliver(event.detail);
  addEventListener("snowglassclilaunch", listener);
  if (window.snowglassCliLaunchOptions) {
    queueMicrotask(() => deliver(window.snowglassCliLaunchOptions!));
  }
  return () => removeEventListener("snowglassclilaunch", listener);
}
