import type { JSX } from "solid-js";

declare module "solid-js" {
  namespace JSX {
    interface IntrinsicElements {
      "tracked-surface": JSX.HTMLAttributes<HTMLElement> & { src?: string };
      "ps2-icon": JSX.HTMLAttributes<HTMLElement> & { "attr:src"?: string };
      "wii-banner": JSX.HTMLAttributes<HTMLElement> & { "attr:src"?: string };
      "animated-image": JSX.HTMLAttributes<HTMLElement> & { "attr:src"?: string };
      "ctr-banner": JSX.HTMLAttributes<HTMLElement> & { "attr:src"?: string };
    }
  }
}
