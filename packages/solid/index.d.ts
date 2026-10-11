import type { HTMLTrackedSurfaceElement } from "@polymer-fe/frontend";

declare module "solid-js" {
  namespace JSX {
    interface IntrinsicElements {
      "tracked-surface": HTMLAttributes<HTMLTrackedSurfaceElement> & { src?: string };
    }
    interface ExplicitAttributes {
      src: string;
    }
  }
}
