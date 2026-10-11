import type { DetailedHTMLProps, HTMLAttributes } from "react";
import type { HTMLTrackedSurfaceElement } from "@polymer-fe/frontend";

declare module "react" {
  namespace JSX {
    interface IntrinsicElements {
      "tracked-surface": DetailedHTMLProps<HTMLAttributes<HTMLTrackedSurfaceElement>, HTMLTrackedSurfaceElement> & { src?: string };
    }
  }
}
