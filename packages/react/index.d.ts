import type { DetailedHTMLProps, HTMLAttributes } from "react";
import type { HTMLTrackedSurfaceElement, HTMLTrackedSurfaceGroupElement } from "@polymer-fe/frontend";

declare module "react" {
  namespace JSX {
    interface IntrinsicElements {
      "tracked-surface": DetailedHTMLProps<HTMLAttributes<HTMLTrackedSurfaceElement>, HTMLTrackedSurfaceElement> & { src?: string };
      "tracked-surface-group": DetailedHTMLProps<HTMLAttributes<HTMLTrackedSurfaceGroupElement>, HTMLTrackedSurfaceGroupElement>;
    }
  }
}
