import type { HTMLTrackedSurfaceElement, HTMLTrackedSurfaceGroupElement } from "@polymer-fe/frontend";

declare module "solid-js" {
  namespace JSX {
    interface IntrinsicElements {
      "tracked-surface": HTMLAttributes<HTMLTrackedSurfaceElement> & { src?: string };
      "tracked-surface-group": HTMLAttributes<HTMLTrackedSurfaceGroupElement>;
    }
    interface ExplicitAttributes {
      src: string;
    }
  }
}
