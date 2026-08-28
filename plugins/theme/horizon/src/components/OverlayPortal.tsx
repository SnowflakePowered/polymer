import type { ParentProps } from "solid-js";
import { Portal } from "solid-js/web";

export function OverlayPortal(props: ParentProps) {
  return <Portal ref={(container) => { container.style.display = "contents"; }}>
    {props.children}
  </Portal>;
}
