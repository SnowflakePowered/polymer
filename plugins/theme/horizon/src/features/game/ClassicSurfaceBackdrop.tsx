import { createEffect, onCleanup, onMount } from "solid-js";
import type { HTMLTrackedSurfaceElement } from "../../../../../../js/polymer";
import "./ClassicSurfaceBackdrop.css";

export function ClassicSurfaceBackdrop(props: { surface?: () => HTMLElement | undefined; revision?: number }) {
  let backdrop!: HTMLDivElement;
  let frame = 0;
  const setSideInset = (sideInset: number) => {
    backdrop.style.setProperty("--surface-side-inset", `${sideInset}px`);
    // Preserve the complete gradient on ultrawide stages, but keep its native
    // 320 CSS-pixel scale when the fitted surface leaves narrower gutters.
    backdrop.style.setProperty("--moire-pattern-width", `${Math.max(320, sideInset)}px`);
  };
  const update = () => {
    const surface = props.surface?.() as HTMLTrackedSurfaceElement | undefined;
    const preferred = surface?.preferredSize;
    const bounds = backdrop.getBoundingClientRect();
    if (!preferred?.width || !preferred.height || !bounds.width || !bounds.height) {
      setSideInset(Math.min(bounds.width / 2, 288));
      return;
    }
    const fittedWidth = Math.min(bounds.width, bounds.height * preferred.width / preferred.height);
    setSideInset(Math.max(0, (bounds.width - fittedWidth) / 2));
  };
  const schedule = () => {
    if (frame) cancelAnimationFrame(frame);
    frame = requestAnimationFrame(() => { frame = 0; update(); });
  };
  createEffect(() => { props.revision; props.surface?.(); schedule(); });
  onMount(() => {
    const observer = new ResizeObserver(schedule);
    observer.observe(backdrop);
    schedule();
    onCleanup(() => { observer.disconnect(); if (frame) cancelAnimationFrame(frame); });
  });
  return <div ref={backdrop} class="horizon-classic-backdrop" aria-hidden="true"><span class="horizon-moire-left" /><span /><span class="horizon-moire-right" /></div>;
}
