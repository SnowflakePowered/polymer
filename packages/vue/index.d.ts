import type { HTMLAttributes } from "vue";

declare module "vue" {
  interface IntrinsicElementAttributes {
    "tracked-surface": HTMLAttributes & { src?: string };
  }
}
