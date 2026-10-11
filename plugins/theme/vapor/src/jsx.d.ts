import type { JSX } from "solid-js";

declare module "solid-js" {
  namespace JSX {
    interface IntrinsicElements {
      "tracked-surface": JSX.HTMLAttributes<HTMLElement> & { src?: string };
    }
  }
}
