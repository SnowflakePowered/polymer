import { createSignal, type JSX } from "solid-js";

export type IconName = "play" | "stop" | "home" | "library" | "settings" | "power" | "search" | "controller" | "volume" | "close" | "back" | "storage" | "info" | "more" | "display" | "notification" | "user" | "key" | "disc" | "check" | "download";

const shapes: Record<IconName, () => JSX.Element> = {
  play: () => <path d="M7 4.5v15l12.5-7.5z" />,
  stop: () => <rect x="5.5" y="5.5" width="13" height="13" rx="1.5" />,
  home: () => <path d="M12 3 2.5 11h3v9.5h5v-6h3v6h5V11h3z" />,
  library: () => <path d="M3 4h4v16H3zM9 4h4v16H9zm5.6.9 3.9-1 4 15.5-3.9 1z" />,
  settings: () => <path d="M10.3 2h3.4l.6 2.6 1.9.8 2.3-1.4 2.4 2.4-1.4 2.3.8 1.9 2.7.6v3.4l-2.7.6-.8 1.9 1.4 2.3-2.4 2.4-2.3-1.4-1.9.8-.6 2.8h-3.4l-.6-2.8-1.9-.8-2.3 1.4-2.4-2.4 1.4-2.3-.8-1.9L2 13.7v-3.4l2.7-.6.8-1.9-1.4-2.3 2.4-2.4 2.3 1.4 1.9-.8zM12 8.4a3.6 3.6 0 1 0 0 7.2 3.6 3.6 0 0 0 0-7.2" fill-rule="evenodd" />,
  power: () => <path d="M11 2h2v10h-2zm-4.6 3 1.4 1.4a7 7 0 1 0 8.4 0L17.6 5A9 9 0 1 1 6.4 5" />,
  search: () => <path d="M10 3a7 7 0 0 1 5.6 11.2l5.6 5.6-1.4 1.4-5.6-5.6A7 7 0 1 1 10 3m0 2a5 5 0 1 0 0 10 5 5 0 0 0 0-10" fill-rule="evenodd" />,
  controller: () => <path d="M7 6h10c2.7 0 4.3 2.2 5 6l.9 5.2c.4 2.6-2.3 3.8-4.2 1.9L15.5 16h-7l-3.2 3.1c-1.9 1.9-4.6.7-4.2-1.9L2 12c.7-3.8 2.3-6 5-6m0 3.2v1.6H5.4v1.6H7V14h1.6v-1.6h1.6v-1.6H8.6V9.2zm9.6 0a1.1 1.1 0 1 0 0 2.2 1.1 1.1 0 0 0 0-2.2m-2.2 2.4a1.1 1.1 0 1 0 0 2.2 1.1 1.1 0 0 0 0-2.2" fill-rule="evenodd" />,
  volume: () => <path d="M3 9h4l5-4.5v15L7 15H3zm12.5-1.6a6 6 0 0 1 0 9.2l-1.3-1.5a4 4 0 0 0 0-6.2zm2.6-3a10 10 0 0 1 0 15.2l-1.3-1.5a8 8 0 0 0 0-12.2z" />,
  close: () => <path d="m5.6 4.2 6.4 6.4 6.4-6.4 1.4 1.4-6.4 6.4 6.4 6.4-1.4 1.4-6.4-6.4-6.4 6.4-1.4-1.4 6.4-6.4-6.4-6.4z" />,
  back: () => <path d="M15.4 4 17 5.6 10.6 12l6.4 6.4-1.6 1.6-8-8z" />,
  storage: () => <path d="M5 4h14l3 10v6H2v-6zm-.6 10h15.2L17.5 6h-11zM16 16.2a1 1 0 1 0 0 2 1 1 0 0 0 0-2" fill-rule="evenodd" />,
  info: () => <path d="M12 2a10 10 0 1 1 0 20 10 10 0 0 1 0-20m-1 8v7h2v-7zm0-3.5V9h2V6.5z" fill-rule="evenodd" />,
  more: () => <path d="M5 10a2 2 0 1 1 0 4 2 2 0 0 1 0-4m7 0a2 2 0 1 1 0 4 2 2 0 0 1 0-4m7 0a2 2 0 1 1 0 4 2 2 0 0 1 0-4" />,
  display: () => <path d="M2 4h20v13H13v2h4v2H7v-2h4v-2H2zm2 2v9h16V6z" fill-rule="evenodd" />,
  notification: () => <path d="M12 2a10 10 0 1 1 0 20 10 10 0 0 1 0-20m-1.2 4.5.3 8h1.8l.3-8zm1.2 9.5a1.3 1.3 0 1 0 0 2.6 1.3 1.3 0 0 0 0-2.6" fill-rule="evenodd" />,
  user: () => <path d="M12 3a4.5 4.5 0 1 1 0 9 4.5 4.5 0 0 1 0-9m0 10.5c4.8 0 8.5 2.4 8.5 5.5V21h-17v-2c0-3.1 3.7-5.5 8.5-5.5" />,
  key: () => <path d="M7.5 9a5.5 5.5 0 0 1 5.3 4H22v3h-2v3h-3v-3h-4.2A5.5 5.5 0 1 1 7.5 9m0 3a2.5 2.5 0 1 0 0 5 2.5 2.5 0 0 0 0-5" fill-rule="evenodd" />,
  disc: () => <path d="M12 2a10 10 0 1 1 0 20 10 10 0 0 1 0-20m0 7a3 3 0 1 0 0 6 3 3 0 0 0 0-6" fill-rule="evenodd" />,
  check: () => <path d="m4 12.5 1.5-1.5L9.5 15 18.5 6 20 7.5 9.5 18z" />,
  download: () => <path d="M11 3h2v9.2l3.6-3.6 1.4 1.4-6 6-6-6 1.4-1.4 3.6 3.6zM3 15h2v4h14v-4h2v6H3z" />,
};

export function Icon(props: { name: IconName; color?: string; size?: number; class?: string }) {
  return <>{(() => {
    const paint = props.color ?? "#ffffff";
    return <svg class={`vp-icon ${props.class ?? ""}`} width={props.size ?? 24} height={props.size ?? 24} viewBox="0 0 24 24" fill={paint} aria-hidden="true">{shapes[props.name]()}</svg>;
  })()}</>;
}

export function useFocused() {
  const [focused, setFocused] = createSignal(false);
  return [focused, { onFocus: () => setFocused(true), onBlur: () => setFocused(false) }] as const;
}
