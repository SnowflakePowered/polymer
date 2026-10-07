import type { JSX } from "solid-js";

declare global {
  interface Navigator {
    clearImageMipChains(): void;
  }
}

declare module "solid-js" {
  namespace JSX {
    interface IntrinsicElements {
      "tracked-surface": JSX.HTMLAttributes<HTMLElement> & { src?: string };
    }
  }
}
